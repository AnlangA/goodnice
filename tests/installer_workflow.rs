#![cfg(windows)]

use std::fs;
use std::path::Path;
use std::process::Command;

use goodnice::installer::{
    CONTEXT_MENU_KEY_NAME, CONTEXT_MENU_LABEL, InstallConfig, RegHive, install, uninstall, win,
};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("goodnice-installer-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
        .unwrap()
}

#[test]
fn full_install_cli_dot_context_menu_and_uninstall_workflow() {
    let dir = tempdir();
    let install_dir = dir.path().join("ProgramFiles").join("Dot");
    let start_menu_dir = dir
        .path()
        .join("ProgramData")
        .join("Start Menu")
        .join("Programs")
        .join("Dot");

    let reg_root = format!("Software\\GoodniceTest_{}", std::process::id());
    let cfg = InstallConfig {
        install_dir: install_dir.clone(),
        start_menu_dir: start_menu_dir.clone(),
        registry_hive: RegHive::CurrentUser,
        env_subkey: format!(r"{reg_root}\Environment"),
        classes_subkey: format!(r"{reg_root}\Classes"),
        uninstall_subkey: format!(r"{reg_root}\Uninstall\GoodniceDot"),
        broadcast_changes: false,
    };

    // 预置初始 PATH，确保安装与卸载不会破坏已有 PATH
    {
        let env_key = win::reg_create_key(win::HKEY_CURRENT_USER, &cfg.env_subkey).unwrap();
        win::reg_set_string(
            env_key.0,
            "Path",
            r"C:\Windows\system32;C:\Windows",
            win::REG_EXPAND_SZ,
        )
        .unwrap();
    }

    let py_bytes = fs::read(env!("CARGO_BIN_EXE_python")).unwrap();
    let uninst_bytes = fs::read(env!("CARGO_BIN_EXE_uninstall")).unwrap();

    // 1. 执行安装
    install(&cfg, &py_bytes, &uninst_bytes).unwrap();

    // 2. 校验安装目录与开始菜单快捷方式
    assert!(cfg.python_exe_path().is_file());
    assert!(cfg.uninstall_exe_path().is_file());
    assert!(cfg.shortcut_path().is_file());
    assert!(cfg.cmd_shim_path().is_file());
    assert!(win::is_file_hidden(&cfg.cmd_shim_path()));

    // 3. 校验 PATH 指向开始菜单目录，且未暴露 python.exe 所在安装目录
    let (reg_path, _) = {
        let env_key = win::reg_create_key(win::HKEY_CURRENT_USER, &cfg.env_subkey).unwrap();
        win::reg_query_string(env_key.0, "Path")
            .unwrap()
            .expect("Path should exist")
    };
    assert!(reg_path.contains(&start_menu_dir.to_string_lossy().into_owned()));
    assert!(!reg_path.contains(&install_dir.to_string_lossy().into_owned()));
    assert!(!reg_path.to_ascii_lowercase().contains("python.exe"));

    // 4. 验证命令行输入 `dot` 调用该工具（且在该 PATH 下不暴露安装目录的 python.exe）
    let work_dir = dir.path().join("work");
    fs::create_dir_all(work_dir.join("nested")).unwrap();
    let cli_file = work_dir.join("cli_doc.pdf");
    fs::write(&cli_file, b"cli-payload-data").unwrap();

    let test_path_env = format!(
        "{};{}",
        start_menu_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let cli_out = Command::new("cmd.exe")
        .current_dir(&work_dir)
        .env("PATH", &test_path_env)
        .args(["/d", "/c", "dot", "cli_doc.pdf"])
        .output()
        .unwrap();
    assert!(
        cli_out.status.success(),
        "dot CLI failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&cli_out.stdout),
        String::from_utf8_lossy(&cli_out.stderr)
    );
    assert_eq!(fs::read(&cli_file).unwrap(), b"cli-payload-data");
    assert!(!work_dir.join("output").exists());

    // 5. 验证右键菜单注册表项（名为“自动解密”，通过 dot.lnk 调用，不暴露 python.exe）
    let file_cmd_reg = query_reg_default(&format!(
        r"{}\*\shell\{}\command",
        cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
    ));
    let dir_cmd_reg = query_reg_default(&format!(
        r"{}\Directory\shell\{}\command",
        cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
    ));
    let bg_cmd_reg = query_reg_default(&format!(
        r"{}\Directory\Background\shell\{}\command",
        cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
    ));
    let label = query_reg_default(&format!(
        r"{}\*\shell\{}",
        cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
    ));
    assert_eq!(label, CONTEXT_MENU_LABEL);
    for cmd_str in [&file_cmd_reg, &dir_cmd_reg, &bg_cmd_reg] {
        assert!(cmd_str.contains("dot.lnk"));
        assert!(!cmd_str.to_ascii_lowercase().contains("python.exe"));
    }

    // 模拟右键单击文件“自动解密”（哪怕 Explorer 当前工作目录在 SystemRoot 下也能正常工作）
    let ctx_file = work_dir.join("context_file.pdf");
    fs::write(&ctx_file, b"context-single-file").unwrap();
    let sys_root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let file_invocation = file_cmd_reg.replace("%1", &ctx_file.to_string_lossy());
    run_shell_command_string(&file_invocation, Path::new(&sys_root));
    assert_eq!(fs::read(&ctx_file).unwrap(), b"context-single-file");
    assert!(!work_dir.join("output").exists());

    // 模拟右键文件夹空白处“自动解密”（递归处理子目录）
    let nested_file = work_dir.join("nested").join("deep.pdf");
    fs::write(&nested_file, b"deep-nested-bytes").unwrap();
    let bg_invocation = bg_cmd_reg.replace("%V", &work_dir.to_string_lossy());
    run_shell_command_string(&bg_invocation, Path::new(&sys_root));
    assert_eq!(fs::read(&nested_file).unwrap(), b"deep-nested-bytes");
    assert!(!work_dir.join("output").exists());

    // 6. 执行卸载并验证清理结果
    uninstall(&cfg).unwrap();

    assert!(!cfg.shortcut_path().exists());
    assert!(!cfg.start_menu_dir.exists());
    assert!(!cfg.python_exe_path().exists());
    assert!(!cfg.install_dir.exists());

    let (after_path, _) = {
        let env_key = win::reg_create_key(win::HKEY_CURRENT_USER, &cfg.env_subkey).unwrap();
        win::reg_query_string(env_key.0, "Path").unwrap().unwrap()
    };
    assert_eq!(after_path, r"C:\Windows\system32;C:\Windows");

    assert!(
        win::reg_open_key_Optional(
            win::HKEY_CURRENT_USER,
            &format!(r"{}\*\shell\{}", cfg.classes_subkey, CONTEXT_MENU_KEY_NAME)
        )
        .unwrap()
        .is_none()
    );

    // 清理测试注册表根键
    win::reg_delete_tree_ignore_missing(win::HKEY_CURRENT_USER, &reg_root).unwrap();
}

fn query_reg_default(subkey: &str) -> String {
    let key = win::reg_open_key_Optional(win::HKEY_CURRENT_USER, subkey)
        .unwrap()
        .expect("registry key should exist");
    win::reg_query_string(key.0, "").unwrap().unwrap().0
}

fn run_shell_command_string(full_cmd: &str, cwd: &Path) {
    use std::os::windows::process::CommandExt;
    // Windows Explorer 直接将注册表中的命令行原样传给 CreateProcessW，不会做 Rust args 的 \" 转义
    let raw_tail = full_cmd
        .strip_prefix("cmd.exe ")
        .expect("command starts with cmd.exe ");
    let out = Command::new("cmd.exe")
        .current_dir(cwd)
        .raw_arg(raw_tail)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "context menu command failed: {full_cmd}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
