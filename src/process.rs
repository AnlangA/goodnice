use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::pathutil::{
    is_tool_temp, is_xlsx, strip_prefix_ci, suffix, xlsx_intermediate_path, xlsx_write_path,
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
    let n = io::copy(&mut from, &mut to)?;
    to.sync_all()?;
    Ok(n)
}

/// 计算源文件对应的最终输出路径（已恢复原本后缀）。
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

    let dest_final = planned_dest(source, cfg.input_root.as_deref(), &cfg.output_dir);
    let original_suffix = suffix(source);
    let originally_xlsx = is_xlsx(source);

    if cfg.dry_run {
        vlog(
            cfg,
            &format!("[dry-run] {} -> {}", source.display(), dest_final.display()),
        );
        return Ok(FileOutcome {
            dest: dest_final,
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

    Ok(FileOutcome {
        dest: dest_final,
        bytes: data.len() as u64,
    })
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
}
