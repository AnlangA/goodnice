use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

use goodnice::installer::enable_utf8_console;

fn main() -> ExitCode {
    #[cfg(windows)]
    enable_utf8_console();

    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("[打包失败] {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<()> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());

    println!("==> [1/3] 编译核心程序 (python.exe) 与卸载程序 (uninstall.exe) [release]...");
    let status = Command::new(&cargo)
        .current_dir(&manifest_dir)
        .args(["build", "--release", "--bin", "python", "--bin", "uninstall"])
        .status()
        .context("无法启动 cargo build")?;
    if !status.success() {
        bail!("编译 python.exe / uninstall.exe 失败");
    }

    let release_dir = manifest_dir.join("target").join("release");
    let built_py = release_dir.join("python.exe");
    let built_uninst = release_dir.join("uninstall.exe");
    ensure_file(&built_py)?;
    ensure_file(&built_uninst)?;

    let staging_dir = manifest_dir.join("target").join("package-staging");
    fs::create_dir_all(&staging_dir)
        .with_context(|| format!("创建暂存目录失败: {}", staging_dir.display()))?;
    let staged_py = staging_dir.join("python.exe");
    let staged_uninst = staging_dir.join("uninstall.exe");
    fs::copy(&built_py, &staged_py).context("复制 python.exe 至暂存目录失败")?;
    fs::copy(&built_uninst, &staged_uninst).context("复制 uninstall.exe 至暂存目录失败")?;

    println!("==> [2/3] 将载荷内嵌编译为独立单文件安装包 (setup.exe) [release]...");
    let status = Command::new(&cargo)
        .current_dir(&manifest_dir)
        .args(["build", "--release", "--bin", "setup"])
        .env("GOODNICE_EMBED_PYTHON_EXE", &staged_py)
        .env("GOODNICE_EMBED_UNINSTALL_EXE", &staged_uninst)
        .status()
        .context("无法启动 cargo build --bin setup")?;
    if !status.success() {
        bail!("编译内嵌安装包 setup.exe 失败");
    }

    let built_setup = release_dir.join("setup.exe");
    ensure_file(&built_setup)?;

    println!("==> [3/3] 输出安装包至 dist/ 目录...");
    let dist_dir = manifest_dir.join("dist");
    fs::create_dir_all(&dist_dir)
        .with_context(|| format!("创建 dist 目录失败: {}", dist_dir.display()))?;

    let dist_setup = dist_dir.join("setup.exe");
    fs::copy(&built_setup, &dist_setup)
        .with_context(|| format!("写出 {} 失败", dist_setup.display()))?;

    let size_kb = fs::metadata(&dist_setup)
        .map(|m| m.len().div_ceil(1024))
        .unwrap_or(0);

    println!();
    println!("========================================");
    println!("  Windows 安装包构建成功！");
    println!("  安装包路径: {} ({} KB)", dist_setup.display(), size_kb);
    println!("========================================");
    println!("说明:");
    println!("  - 双击或运行 `dist\\setup.exe` 即可执行系统级安装（自动请求管理员权限）");
    println!("  - 安装后自动创建开始菜单快捷方式 `dot.lnk`、注入 PATH (`dot`)、注入右键菜单“自动解密”");
    Ok(())
}

fn ensure_file(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("未找到构建产物: {}", path.display());
    }
    Ok(())
}
