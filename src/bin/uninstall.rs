#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::Parser;

use goodnice::installer::{InstallConfig, is_elevated, relaunch_as_admin, uninstall, win};

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

    /// 静默卸载，不弹出确认与完成对话框
    #[arg(short, long)]
    quiet: bool,

    /// 不自动触发 UAC 提权
    #[arg(long)]
    no_elevate: bool,
}

fn main() -> ExitCode {
    let args = UninstallArgs::parse();
    let quiet = args.quiet;

    match run(args) {
        Ok(code) => code,
        Err(err) => {
            #[cfg(windows)]
            if !quiet {
                win::message_box(
                    0,
                    &format!("卸载过程中发生错误：\n\n{err:#}"),
                    "Dot (自动解密工具) 卸载程序",
                    win::MB_OK | win::MB_ICONHAND,
                );
            }
            let _ = quiet;
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run(args: UninstallArgs) -> Result<ExitCode> {
    if !args.no_elevate && !is_elevated() {
        let code = relaunch_as_admin()?;
        return Ok(ExitCode::from(code as u8));
    }

    if !args.quiet {
        let confirm = win::message_box(
            0,
            "您确定要从计算机中完全卸载 Dot (自动解密工具) 及其所有配置（开始菜单快捷方式、PATH 环境变量、右键“自动解密”菜单）吗？",
            "Dot (自动解密工具) 卸载确认",
            win::MB_YESNO | win::MB_ICONQUESTION,
        );
        if confirm != win::IDYES {
            return Ok(ExitCode::SUCCESS);
        }
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

    uninstall(&cfg)?;

    if !args.quiet {
        win::message_box(
            0,
            "Dot (自动解密工具) 已成功从您的计算机中卸载。",
            "Dot (自动解密工具) 卸载完成",
            win::MB_OK | win::MB_ICONINFORMATION,
        );
    }

    Ok(ExitCode::SUCCESS)
}

#[cfg(not(windows))]
fn run(_args: UninstallArgs) -> Result<ExitCode> {
    bail!("此卸载程序仅支持 Windows 系统");
}
