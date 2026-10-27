use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;

use anyhow::{Context, Result, bail};

use crate::pathutil::{
    is_within, is_xlsx, strip_prefix_ci, suffix, xlsx_intermediate_path,
};
use crate::powershell::{self, PowerShellSession};

/// Windows 内核页缓存临时标志 + 隐藏属性：
/// `FILE_ATTRIBUTE_TEMPORARY (0x0100)` 指示 Windows Cache Manager 将临时 `.xlsx` 句柄数据驻留在内存缓存中，不主动刷写到物理磁盘；
/// `FILE_ATTRIBUTE_HIDDEN (0x0002)` 在资源管理器中隐藏该临时句柄。
#[cfg(windows)]
const TEMP_CACHE_FILE_FLAGS: u32 = 0x0000_0100 | 0x0000_0002;

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
        // 仅删除本次 create_new 得到的副本；成功路径会先显式删除并 disarm。
        if self.armed && self.path.exists() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// 创建内核内存缓存的临时 `.xlsx` 句柄（存在则失败，避免覆盖用户已有的 `.xlsx`），不强制刷盘。
fn copy_new(src: &Path, dst: &Path) -> io::Result<u64> {
    let mut from = fs::File::open(src)?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(windows)]
    {
        opts.custom_flags(TEMP_CACHE_FILE_FLAGS);
    }
    let mut to = opts.open(dst)?;
    let mut guard = DeleteOnDrop::new(dst.to_path_buf());
    let n = io::copy(&mut from, &mut to)?;
    guard.disarm();
    Ok(n)
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
    session: Option<&mut PowerShellSession>,
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

    // 第 1 步：非 xlsx 则创建内存页缓存的临时 xlsx 句柄（FILE_ATTRIBUTE_TEMPORARY | FILE_ATTRIBUTE_HIDDEN）
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
        let sibling = crate::pathutil::with_suffix(source, ".xlsx");
        let xlsx_file = match copy_new(source, &sibling) {
            Ok(_) => sibling,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                let marked = xlsx_intermediate_path(source);
                copy_new(source, &marked).with_context(|| {
                    format!(
                        "创建临时 xlsx 缓存失败: {} -> {}",
                        source.display(),
                        marked.display()
                    )
                })?;
                marked
            }
            Err(err) => {
                return Err(err).with_context(|| {
                    format!(
                        "创建临时 xlsx 缓存失败: {} -> {}",
                        source.display(),
                        sibling.display()
                    )
                });
            }
        };
        vlog(
            cfg,
            &format!(
                "已在内存缓存创建临时 xlsx 句柄: {} (原文件保留)",
                xlsx_file.display()
            ),
        );
        tmp_guard = Some(DeleteOnDrop::new(xlsx_file.clone()));
        xlsx_file
    };

    // 第 2 步：将解密后的字节流读入内存，全程在内存中经管道传给 PowerShell 校验并直接将最终文件存盘
    vlog(cfg, &format!("读取字节流至内存: {}", read_path.display()));
    let data =
        fs::read(&read_path).with_context(|| format!("读取失败: {}", read_path.display()))?;
    vlog(cfg, &format!("内存已缓存 {} 字节", data.len()));

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

    // 第 3 步：最终文件存盘完成后，立即删除临时 xlsx 缓存句柄
    if let Some(guard) = tmp_guard.as_mut() {
        remove_file_best_effort(&guard.path)
            .with_context(|| format!("删除临时文件失败: {}", guard.path.display()))?;
        vlog(
            cfg,
            &format!("已删除临时 xlsx 缓存文件: {}", guard.path.display()),
        );
        guard.disarm();
    }

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
    fn copy_new_marks_temporary_and_hidden_attributes_in_ram_cache() {
        use std::os::windows::fs::MetadataExt;
        let dir = tempdir().unwrap();
        let src = dir.path().join("a.pdf");
        let dst = dir.path().join("a.xlsx");
        fs::write(&src, b"in-memory-cache-payload").unwrap();

        let n = copy_new(&src, &dst).unwrap();
        assert_eq!(n, 23);
        let attrs = fs::metadata(&dst).unwrap().file_attributes();
        // FILE_ATTRIBUTE_TEMPORARY (0x100) | FILE_ATTRIBUTE_HIDDEN (0x2)
        assert_ne!(attrs & 0x0000_0100, 0, "應帶有 FILE_ATTRIBUTE_TEMPORARY 標誌");
        assert_ne!(attrs & 0x0000_0002, 0, "應帶有 FILE_ATTRIBUTE_HIDDEN 標誌");
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
