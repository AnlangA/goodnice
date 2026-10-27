use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use goodnice::installer::{
    InstallConfig, enable_utf8_console, is_elevated, relaunch_as_admin, uninstall,
    wait_for_enter_if_interactive,
};

#[derive(Parser, Debug)]
#[command(
    name = "uninstall",
    version,
    about = "Dot (自动解密工具) Windows 卸载程序"
)]
struct UninstallArgs {
    /// 自定义安装目录（默认：C:\Program Files\Dot）
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,

    /// 自定义开始菜单快捷方式目录（默认：C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot）
    #[arg(long, value_name = "DIR")]
    start_menu_dir: Option<PathBuf>,

    /// 静默卸载，完成后不等待回车
    #[arg(short, long)]
    quiet: bool,

    /// 不自动触发 UAC 提权
    #[arg(long)]
    no_elevate: bool,
}

fn main() -> ExitCode {
    #[cfg(windows)]
    enable_utf8_console();

    let args = UninstallArgs::parse();
    let quiet = args.quiet;

    match run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("\n[卸载失败] {err:#}");
            wait_for_enter_if_interactive(quiet);
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run(args: UninstallArgs) -> Result<ExitCode> {
    if !args.no_elevate && !is_elevated() {
        println!("正在请求管理员权限 (UAC) 以执行系统级卸载...");
        let code = relaunch_as_admin()?;
        return Ok(ExitCode::from(code as u8));
    }

    let mut cfg = InstallConfig::system_default();
    if let Some(dir) = args.install_dir {
        cfg.install_dir = dir;
    } else if let Ok(self_exe) = std::env::current_exe()
        && let Some(parent) = self_exe.parent()
        && parent.join("python.exe").exists()
    {
        cfg.install_dir = parent.to_path_buf();
    }

    if let Some(sm_dir) = args.start_menu_dir {
        cfg.start_menu_dir = sm_dir;
    }

    println!("========================================");
    println!("  正在卸载 Dot (自动解密工具)");
    println!("========================================");

    uninstall(&cfg)?;

    println!("  - 已移除“自动解密”右键菜单");
    println!("  - 已从系统 PATH 环境变量移除快捷方式目录");
    println!("  - 已删除开始菜单快捷方式: {}", cfg.shortcut_path().display());
    println!("  - 已清理安装目录: {}", cfg.install_dir.display());
    println!("----------------------------------------");
    println!("卸载完成！");

    wait_for_enter_if_interactive(args.quiet);
    Ok(ExitCode::SUCCESS)
}

#[cfg(not(windows))]
fn run(_args: UninstallArgs) -> Result<ExitCode> {
    bail!("此卸载程序仅支持 Windows 系统");
}
