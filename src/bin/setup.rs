use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;

use goodnice::installer::{
    InstallConfig, PYTHON_EXE_NAME, UNINSTALL_EXE_NAME, enable_utf8_console, install, is_elevated,
    relaunch_as_admin, wait_for_enter_if_interactive,
};

include!(concat!(env!("OUT_DIR"), "/embedded_payload.rs"));

#[derive(Parser, Debug)]
#[command(
    name = "setup",
    version,
    about = "Dot (自动解密工具) Windows 系统级安装程序"
)]
struct SetupArgs {
    /// 自定义安装目录（默认：C:\Program Files\Dot）
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,

    /// 自定义开始菜单快捷方式目录（默认：C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot）
    #[arg(long, value_name = "DIR")]
    start_menu_dir: Option<PathBuf>,

    /// 静默安装，完成后不等待回车
    #[arg(short, long)]
    quiet: bool,

    /// 不自动触发 UAC 提权（供测试或已提权环境使用）
    #[arg(long)]
    no_elevate: bool,
}

fn main() -> ExitCode {
    #[cfg(windows)]
    enable_utf8_console();

    let args = SetupArgs::parse();
    let quiet = args.quiet;

    match run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("\n[安装失败] {err:#}");
            wait_for_enter_if_interactive(quiet);
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run(args: SetupArgs) -> Result<ExitCode> {
    if !args.no_elevate && !is_elevated() {
        println!("正在请求管理员权限 (UAC) 以执行系统级安装...");
        let code = relaunch_as_admin()?;
        return Ok(ExitCode::from(code as u8));
    }

    let mut cfg = InstallConfig::system_default();
    if let Some(dir) = args.install_dir {
        cfg.install_dir = dir;
    }
    if let Some(sm_dir) = args.start_menu_dir {
        cfg.start_menu_dir = sm_dir;
    }

    println!("========================================");
    println!("  正在安装 Dot (自动解密工具) v{}", env!("CARGO_PKG_VERSION"));
    println!("========================================");

    let (py_bytes, uninst_bytes) = load_payload_binaries()?;
    install(&cfg, &py_bytes, &uninst_bytes)?;

    println!("  [1/4] 已释放核心程序: {}", cfg.python_exe_path().display());
    println!("  [2/4] 已创建开始菜单快捷方式: {}", cfg.shortcut_path().display());
    println!(
        "  [3/4] 已将快捷方式目录注入系统 PATH: {}",
        cfg.start_menu_dir.display()
    );
    println!("  [4/4] 已注入文件与文件夹右键菜单: 自动解密");
    println!("----------------------------------------");
    println!("安装完成！");
    println!("  - 命令行使用: 打开新的终端输入 `dot <文件或目录>`");
    println!("  - 右键使用:   在文件、文件夹或目录空白处右键选择“自动解密”");
    println!("  - 卸载程序:   {}", cfg.uninstall_exe_path().display());

    wait_for_enter_if_interactive(args.quiet);
    Ok(ExitCode::SUCCESS)
}

#[cfg(windows)]
fn load_payload_binaries() -> Result<(Vec<u8>, Vec<u8>)> {
    if let (Some(py), Some(uninst)) = (EMBEDDED_PYTHON_EXE, EMBEDDED_UNINSTALL_EXE) {
        return Ok((py.to_vec(), uninst.to_vec()));
    }

    let self_exe = env::current_exe().context("无法获取安装程序自身路径")?;
    let parent = self_exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("无法定位安装程序所在目录"))?;

    let py_path = parent.join(PYTHON_EXE_NAME);
    let uninst_path = parent.join(UNINSTALL_EXE_NAME);

    if !py_path.is_file() {
        bail!(
            "未找到内置或同目录的 {} (期望路径: {})。请使用 `cargo run --bin package` 或 `.\\package.ps1` 构建完整安装包。",
            PYTHON_EXE_NAME,
            py_path.display()
        );
    }
    if !uninst_path.is_file() {
        bail!(
            "未找到内置或同目录的 {} (期望路径: {})。请使用 `cargo run --bin package` 或 `.\\package.ps1` 构建完整安装包。",
            UNINSTALL_EXE_NAME,
            uninst_path.display()
        );
    }

    let py_bytes =
        fs::read(&py_path).with_context(|| format!("读取失败: {}", py_path.display()))?;
    let uninst_bytes =
        fs::read(&uninst_path).with_context(|| format!("读取失败: {}", uninst_path.display()))?;
    Ok((py_bytes, uninst_bytes))
}

#[cfg(not(windows))]
fn run(_args: SetupArgs) -> Result<ExitCode> {
    bail!("此安装程序仅支持 Windows 系统");
}
