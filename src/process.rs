use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::pathutil::{
    is_tool_temp, is_within, is_xlsx, strip_prefix_ci, suffix, xlsx_intermediate_path,
    xlsx_write_path,
};
use crate::powershell;

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
        // 仅删除本次 create_new 得到的副本；成功路径会先 disarm。
        if self.armed && self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// 只创建新文件，存在则失败，避免覆盖用户已有的 `.xlsx`。
fn copy_new(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut from = fs::File::open(src)?;
    let mut to = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    let mut guard = DeleteOnDrop::new(dst.to_path_buf());
    let n = io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    guard.disarm();
    Ok(n)
}

/// 计算源文件在输出目录中的路径（恢复原后缀后，还会移回源路径）。
pub fn planned_dest(source: &Path, input_root: Option<&Path>, output_dir: &Path) -> PathBuf {
    match input_root {
        Some(root) => match strip_prefix_ci(source, root) {
            Some(rel) if !rel.as_os_str().is_empty() => output_dir.join(rel),
            _ => output_dir.join(source.file_name().unwrap_or_default()),
        },
        None => output_dir.join(source.file_name().unwrap_or_default()),
    }
}

pub fn process_file(source: &Path, cfg: &ProcessConfig) -> Result<FileOutcome> {
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
                "[dry-run] {} -> {}；校验并恢复后缀后，删除源文件，再移回 {}",
                source.display(),
                dest_final.display(),
                source.display()
            ),
        );
        return Ok(FileOutcome {
            dest: source.to_path_buf(),
            bytes: fs::metadata(source).map(|m| m.len()).unwrap_or(0),
        });
    }

    // 第 1 步：非 xlsx 则记录原格式，复制一份并把后缀改成 xlsx（原文件保留）
    let mut tmp_guard: Option<DeleteOnDrop> = None;
    let read_path = if originally_xlsx {
        vlog(cfg, "已是 xlsx 格式，直接进入读取");
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
        let sibling = crate::pathutil::with_suffix(source, ".xlsx");
        let xlsx_file = match copy_new(source, &sibling) {
            Ok(_) => sibling,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let marked = xlsx_intermediate_path(source);
                copy_new(source, &marked).with_context(|| {
                    format!(
                        "复制为 xlsx 失败: {} -> {}",
                        source.display(),
                        marked.display()
                    )
                })?;
                marked
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "复制为 xlsx 失败: {} -> {}",
                        source.display(),
                        sibling.display()
                    )
                });
            }
        };
        vlog(
            cfg,
            &format!(
                "已复制原文件并改后缀为 xlsx: {} (原文件保留)",
                xlsx_file.display()
            ),
        );
        tmp_guard = Some(DeleteOnDrop::new(xlsx_file.clone()));
        xlsx_file
    };

    // 第 2 步：读取字节流，完全通过 PowerShell 写入输出目录
    vlog(cfg, &format!("读取: {}", read_path.display()));
    let data =
        fs::read(&read_path).with_context(|| format!("读取失败: {}", read_path.display()))?;
    vlog(cfg, &format!("读取到 {} 字节", data.len()));

    let dest_write = if originally_xlsx {
        dest_final.clone()
    } else {
        xlsx_write_path(&dest_final)
    };
    let dest_dir = dest_write
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    powershell::write_bytes_via_powershell(
        &cfg.powershell,
        &data,
        &dest_dir,
        &dest_write,
        cfg.verbose,
    )?;
    vlog(
        cfg,
        "验证通过: 输出文件字节流与读取数据完全一致(由 PowerShell 校验)",
    );

    // 第 3 步：原本不是 xlsx，则把输出文件改回原本格式，并删除中间 xlsx 副本
    if !originally_xlsx {
        if dest_write != dest_final {
            // 直接 rename 可能被系统拦截，改用复制+删除（与 Python 脚本相同）
            fs::copy(&dest_write, &dest_final).with_context(|| {
                format!(
                    "改回原本格式失败: {} -> {}",
                    dest_write.display(),
                    dest_final.display()
                )
            })?;
            remove_tool_temp(&dest_write)?;
            vlog(
                cfg,
                &format!("输出文件已改回原本格式: {}", dest_final.display()),
            );
        }
        if let Some(guard) = tmp_guard.as_mut() {
            remove_file_best_effort(&guard.path)
                .with_context(|| format!("删除中间文件失败: {}", guard.path.display()))?;
            vlog(
                cfg,
                &format!("已删除中间的 xlsx 副本: {}", guard.path.display()),
            );
            guard.disarm();
        }
    }

    // 第 4、5 步：原流程全部成功后，删除源文件，再把输出文件移回原路径。
    replace_source(source, &dest_final).with_context(|| {
        format!(
            "替换源文件失败: {}；输出文件保留在 {}",
            source.display(),
            dest_final.display()
        )
    })?;
    vlog(
        cfg,
        &format!("已删除源文件并移回输出文件: {}", source.display()),
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

fn replace_source(source: &Path, output: &Path) -> Result<()> {
    if !output.is_file() {
        bail!("输出文件不存在: {}", output.display());
    }
    fs::remove_file(source).with_context(|| format!("删除源文件失败: {}", source.display()))?;
    if fs::rename(output, source).is_err() {
        // 跨磁盘或系统不允许 rename 时，用创建新文件、复制、删除实现移动。
        copy_new(output, source).with_context(|| {
            format!(
                "移回源目录失败: {} -> {}",
                output.display(),
                source.display()
            )
        })?;
        fs::remove_file(output)
            .with_context(|| format!("文件已移回，删除输出副本失败: {}", output.display()))?;
    }
    Ok(())
}

/// 第 6 步：批量处理成功后从内向外删除空输出目录，不跟随目录链接。
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

fn remove_tool_temp(path: &Path) -> Result<()> {
    if !is_tool_temp(path) {
        bail!("拒绝删除非本工具中间文件: {}", path.display());
    }
    remove_file_best_effort(path).with_context(|| format!("删除中间文件失败: {}", path.display()))
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

    #[test]
    fn replaces_source_with_output_and_removes_output_file() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        let output = dir.path().join("output.pdf");
        fs::write(&source, b"original").unwrap();
        fs::write(&output, b"processed").unwrap();

        replace_source(&source, &output).unwrap();

        assert_eq!(fs::read(&source).unwrap(), b"processed");
        assert!(!output.exists());
    }

    #[test]
    fn replaces_source_from_a_separate_location() {
        // 系统临时目录与仓库位于不同磁盘时，验证跨磁盘移动的复制回退。
        let source_dir =
            tempfile::tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target")).unwrap();
        let output_dir = tempdir().unwrap();
        let source = source_dir.path().join("a.pdf");
        let output = output_dir.path().join("a.pdf");
        fs::write(&source, b"original").unwrap();
        fs::write(&output, b"processed").unwrap();

        replace_source(&source, &output).unwrap();

        assert_eq!(fs::read(&source).unwrap(), b"processed");
        assert!(!output.exists());
    }

    #[test]
    fn missing_output_does_not_delete_source() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("a.pdf");
        fs::write(&source, b"original").unwrap();

        assert!(replace_source(&source, &dir.path().join("missing.pdf")).is_err());
        assert_eq!(fs::read(source).unwrap(), b"original");
    }

    #[test]
    fn failed_source_deletion_preserves_output() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("directory");
        let output = dir.path().join("output.pdf");
        fs::create_dir(&source).unwrap();
        fs::write(&output, b"processed").unwrap();

        assert!(replace_source(&source, &output).is_err());
        assert_eq!(fs::read(output).unwrap(), b"processed");
        assert!(source.is_dir());
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
