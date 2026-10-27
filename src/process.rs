use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::pathutil::{is_within, is_xlsx, ram_temp_xlsx_path, strip_prefix_ci, suffix};
use crate::powershell::{self, PowerShellSession};

pub struct ProcessConfig {
    pub output_dir: PathBuf,
    /// 目录模式：源文件相对此根目录的路径会映射到输出目录，以保留子目录结构。
    /// 单文件模式：为 `None`，输出到 `output_dir / 文件名`。
    pub input_root: Option<PathBuf>,
    pub powershell: OsString,
    pub verbose: bool,
    pub dry_run: bool,
}

pub struct FileOutcome {
    pub dest: PathBuf,
    pub bytes: u64,
}

struct DeleteOnDrop {
    path: PathBuf,
    armed: bool,
}

impl DeleteOnDrop {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DeleteOnDrop {
    fn drop(&mut self) {
        if self.armed {
            cleanup_temp_xlsx(&self.path);
        }
    }
}

fn cleanup_temp_xlsx(path: &Path) {
    let _ = fs::remove_file(path);
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// 计算源文件在回退输出目录中的路径（仅当源文件被占用无法原位落盘时使用）。
pub fn planned_dest(source: &Path, input_root: Option<&Path>, output_dir: &Path) -> PathBuf {
    match input_root {
        Some(root) => match strip_prefix_ci(source, root) {
            Some(rel) if !rel.as_os_str().is_empty() => output_dir.join(rel),
            _ => output_dir.join(source.file_name().unwrap_or_default()),
        },
        None => output_dir.join(source.file_name().unwrap_or_default()),
    }
}

#[cfg(test)]
pub fn process_file(source: &Path, cfg: &ProcessConfig) -> Result<FileOutcome> {
    process_file_with_session(source, cfg, None)
}

pub fn process_file_with_session(
    source: &Path,
    cfg: &ProcessConfig,
    mut session: Option<&mut PowerShellSession>,
) -> Result<FileOutcome> {
    if !source.is_file() {
        bail!("文件不存在: {}", source.display());
    }
    validate_output_dir(source, &cfg.output_dir)?;

    let dest_final = planned_dest(source, cfg.input_root.as_deref(), &cfg.output_dir);
    let original_suffix = suffix(source);
    let originally_xlsx = is_xlsx(source);

    if cfg.dry_run {
        vlog(
            cfg,
            &format!(
                "[dry-run] {} -> 内存读取字节流并校验后直接存盘替换 {}",
                source.display(),
                source.display()
            ),
        );
        return Ok(FileOutcome {
            dest: source.to_path_buf(),
            bytes: fs::metadata(source).map(|m| m.len()).unwrap_or(0),
        });
    }

    // 第 1 步：非 xlsx 文件通过不受加密策略控制的 PowerShell 在系统临时区构建 .xlsx 句柄
    // （避免在用户源目录产生任何中间文件，同时防止 python.exe 写入 .xlsx 时触发亿赛通二次加密）
    let mut tmp_guard: Option<DeleteOnDrop> = None;
    let read_path = if originally_xlsx {
        vlog(cfg, "已是 xlsx 格式，直接进入内存读取");
        source.to_path_buf()
    } else {
        vlog(
            cfg,
            &format!(
                "记录原始格式: {}",
                if original_suffix.is_empty() {
                    "(无扩展名)"
                } else {
                    &original_suffix
                }
            ),
        );
        let temp_xlsx = ram_temp_xlsx_path(source);
        let guard = DeleteOnDrop::new(temp_xlsx.clone());
        match session.as_deref_mut() {
            Some(sess) => {
                sess.stage_temp_xlsx(source, &temp_xlsx, cfg.verbose)?;
            }
            None => {
                powershell::stage_temp_xlsx_via_powershell(
                    &cfg.powershell,
                    source,
                    &temp_xlsx,
                    cfg.verbose,
                )?;
            }
        }
        tmp_guard = Some(guard);
        temp_xlsx
    };

    // 第 2 步：由具备亿赛通表格解密权限的 python.exe 将字节流读入内存（触发透明解密），并立即清理系统临时区句柄
    vlog(cfg, &format!("读取解密字节流至内存: {}", read_path.display()));
    let data =
        fs::read(&read_path).with_context(|| format!("读取失败: {}", read_path.display()))?;
    vlog(cfg, &format!("内存已缓存 {} 字节", data.len()));

    if let Some(guard) = tmp_guard.as_mut() {
        remove_file_best_effort(&guard.path)
            .with_context(|| format!("删除临时文件失败: {}", guard.path.display()))?;
        if let Some(parent) = guard.path.parent() {
            let _ = fs::remove_dir(parent);
        }
        vlog(
            cfg,
            &format!("已清理临时 xlsx 句柄: {}", guard.path.display()),
        );
        guard.disarm();
    }

    // 第 3 步：将内存中的解密字节流通过管道传给 PowerShell 校验并直接存盘至原路径
    let dest_dir = dest_final
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    match session {
        Some(sess) => {
            sess.write_final_file(&data, source, &dest_dir, &dest_final, cfg.verbose)?;
        }
        None => {
            powershell::write_bytes_via_powershell(
                &cfg.powershell,
                &data,
                source,
                &dest_dir,
                &dest_final,
                cfg.verbose,
            )?;
        }
    }
    vlog(
        cfg,
        &format!(
            "验证通过并已将最终文件存盘: {} (由 PowerShell 内存校验)",
            source.display()
        ),
    );

    Ok(FileOutcome {
        dest: source.to_path_buf(),
        bytes: data.len() as u64,
    })
}

/// 输出目录不能包含源文件，否则写入、删除源文件或清理目录会破坏数据。
pub fn validate_output_dir(source: &Path, output_dir: &Path) -> Result<()> {
    let resolved_overlap = match (fs::canonicalize(source), fs::canonicalize(output_dir)) {
        (Ok(source), Ok(output_dir)) => is_within(&source, &output_dir),
        _ => false,
    };
    if is_within(source, output_dir) || resolved_overlap {
        bail!(
            "源路径位于输出目录内: {}。请使用 -o 指定其他输出目录",
            source.display()
        );
    }
    Ok(())
}

/// 批量处理成功后从内向外删除空输出目录，不跟随目录链接。
/// 返回 false 表示仍有文件或链接，需要保留目录。
pub fn cleanup_output_dir(output_dir: &Path) -> Result<bool> {
    let metadata = match fs::symlink_metadata(output_dir) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err).context("读取输出目录失败"),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(false);
    }
    for entry in fs::read_dir(output_dir)
        .with_context(|| format!("读取输出目录失败: {}", output_dir.display()))?
    {
        let entry = entry.context("读取输出目录项失败")?;
        if entry.file_type()?.is_dir() {
            cleanup_output_dir(&entry.path())?;
        }
    }
    match fs::remove_dir(output_dir) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
        Err(err) => Err(err).with_context(|| format!("删除输出目录失败: {}", output_dir.display())),
    }
}

fn remove_file_best_effort(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

fn vlog(cfg: &ProcessConfig, msg: &str) {
    if cfg.verbose {
        println!("    {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_config(output_dir: PathBuf, dry_run: bool) -> ProcessConfig {
        ProcessConfig {
            output_dir,
            input_root: None,
            powershell: OsString::from("goodnice-missing-powershell"),
            verbose: false,
            dry_run,
        }
    }

    #[test]
    fn single_file_dest_uses_basename() {
        let source = Path::new("docs").join("sub").join("a.pdf");
        let dest = planned_dest(&source, None, Path::new("output"));
        assert_eq!(dest, PathBuf::from("output").join("a.pdf"));
    }

    #[test]
    fn directory_dest_keeps_relative_structure() {
        let root = Path::new("docs");
        let source = root.join("sub").join("a.pdf");
        let dest = planned_dest(&source, Some(root), Path::new("output"));
        assert_eq!(dest, PathBuf::from("output").join("sub").join("a.pdf"));
    }

    #[cfg(windows)]
    #[test]
    fn stage_temp_xlsx_marks_temporary_and_hidden_in_system_temp() {
        use std::os::windows::fs::MetadataExt;
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.pdf");
        fs::write(&src, b"in-memory-cache-payload").unwrap();

        let dst = ram_temp_xlsx_path(&src);
        assert_ne!(dst.parent(), Some(dir.path()));
        let mut sess = PowerShellSession::spawn_auto().unwrap();
        sess.stage_temp_xlsx(&src, &dst, false).unwrap();

        let attrs = fs::metadata(&dst).unwrap().file_attributes();
        // FILE_ATTRIBUTE_TEMPORARY (0x100) | FILE_ATTRIBUTE_HIDDEN (0x2)
        assert_ne!(attrs & 0x0000_0100, 0, "应带有 FILE_ATTRIBUTE_TEMPORARY 标志");
        assert_ne!(attrs & 0x0000_0002, 0, "应带有 FILE_ATTRIBUTE_HIDDEN 标志");
        cleanup_temp_xlsx(&dst);
    }

    #[test]
    fn cleanup_removes_nested_empty_output_directories() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("output");
        fs::create_dir_all(output.join("sub").join("nested")).unwrap();
        fs::create_dir_all(output.join("other")).unwrap();

        assert!(cleanup_output_dir(&output).unwrap());
        assert!(!output.exists());
        assert!(dir.path().exists());
        assert!(cleanup_output_dir(&output).unwrap());
    }

    #[test]
    fn cleanup_preserves_remaining_files() {
        let dir = tempdir().unwrap();
        let output = dir.path().join("output");
        fs::create_dir_all(output.join("sub")).unwrap();
        let remaining = output.join("sub").join("keep.txt");
        fs::write(&remaining, b"keep").unwrap();

        assert!(!cleanup_output_dir(&output).unwrap());
        assert_eq!(fs::read(remaining).unwrap(), b"keep");
    }

    #[test]
    fn dry_run_keeps_source_and_existing_output_untouched() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        let output = dir.path().join("output");
        fs::write(&source, b"original").unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(output.join("a.pdf"), b"existing").unwrap();

        let outcome = process_file(&source, &test_config(output.clone(), true)).unwrap();

        assert_eq!(outcome.dest, source);
        assert_eq!(outcome.bytes, 8);
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read(output.join("a.pdf")).unwrap(), b"existing");
        assert!(!source.with_extension("xlsx").exists());
    }

    #[test]
    fn source_inside_output_is_rejected_before_writing() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("a.xlsx");
        fs::write(&source, b"original").unwrap();

        assert!(process_file(&source, &test_config(dir.path().to_path_buf(), false)).is_err());
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[test]
    fn failed_powershell_keeps_source_and_cleans_intermediate() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        let output = dir.path().join("output");
        fs::write(&source, b"original").unwrap();

        assert!(process_file(&source, &test_config(output, false)).is_err());
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert!(!source.with_extension("xlsx").exists());
    }
}
