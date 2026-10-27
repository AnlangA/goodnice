use std::env;
use std::ffi::c_void;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use anyhow::{Context, Result, anyhow, bail};

pub const APP_NAME: &str = "Dot";
pub const SHORTCUT_NAME: &str = "dot.lnk";
pub const SHIM_NAME: &str = "dot.cmd";
pub const VBS_SHIM_NAME: &str = "dot.vbs";
pub const ICON_FILE_NAME: &str = "dot.ico";
pub const PYTHON_EXE_NAME: &str = "python.exe";
pub const UNINSTALL_EXE_NAME: &str = "uninstall.exe";
pub const CONTEXT_MENU_KEY_NAME: &str = "GoodniceDot";
pub const CONTEXT_MENU_LABEL: &str = "自动解密";

pub const SYSTEM_ENV_SUBKEY: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
pub const SYSTEM_CLASSES_SUBKEY: &str = r"Software\Classes";
pub const SYSTEM_UNINSTALL_SUBKEY: &str =
    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\GoodniceDot";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegHive {
    LocalMachine,
    CurrentUser,
}

impl RegHive {
    #[cfg(windows)]
    fn raw_handle(self) -> isize {
        match self {
            Self::LocalMachine => win::HKEY_LOCAL_MACHINE,
            Self::CurrentUser => win::HKEY_CURRENT_USER,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallConfig {
    pub install_dir: PathBuf,
    pub start_menu_dir: PathBuf,
    pub registry_hive: RegHive,
    pub env_subkey: String,
    pub classes_subkey: String,
    pub uninstall_subkey: String,
    pub enable_path_env: bool,
    pub enable_context_menu: bool,
    pub broadcast_changes: bool,
}

impl InstallConfig {
    pub fn system_default() -> Self {
        let program_files = env::var_os("ProgramFiles")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let program_data = env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));

        Self {
            install_dir: program_files.join(APP_NAME),
            start_menu_dir: program_data
                .join("Microsoft")
                .join("Windows")
                .join("Start Menu")
                .join("Programs")
                .join(APP_NAME),
            registry_hive: RegHive::LocalMachine,
            env_subkey: SYSTEM_ENV_SUBKEY.to_string(),
            classes_subkey: SYSTEM_CLASSES_SUBKEY.to_string(),
            uninstall_subkey: SYSTEM_UNINSTALL_SUBKEY.to_string(),
            enable_path_env: true,
            enable_context_menu: true,
            broadcast_changes: true,
        }
    }

    pub fn python_exe_path(&self) -> PathBuf {
        self.install_dir.join(PYTHON_EXE_NAME)
    }

    pub fn uninstall_exe_path(&self) -> PathBuf {
        self.install_dir.join(UNINSTALL_EXE_NAME)
    }

    pub fn shortcut_path(&self) -> PathBuf {
        self.start_menu_dir.join(SHORTCUT_NAME)
    }

    pub fn cmd_shim_path(&self) -> PathBuf {
        self.start_menu_dir.join(SHIM_NAME)
    }

    pub fn vbs_shim_path(&self) -> PathBuf {
        self.start_menu_dir.join(VBS_SHIM_NAME)
    }

    pub fn icon_path(&self) -> PathBuf {
        self.start_menu_dir.join(ICON_FILE_NAME)
    }
}

/// 生成文件右键菜单命令（通过 wscript.exe 静默无弹窗启动开始菜单目录下的 dot.vbs -> dot.lnk，不暴露 python.exe）
pub fn file_context_command(vbs_shim_path: &Path) -> String {
    format!(
        "wscript.exe //B //NoLogo \"{}\" \"%1\"",
        vbs_shim_path.display()
    )
}

/// 生成目录及目录空白处右键菜单命令（默认带 -r 递归处理，通过 wscript.exe 静默无弹窗调用 dot.vbs -> dot.lnk）
pub fn directory_context_command(vbs_shim_path: &Path) -> String {
    format!(
        "wscript.exe //B //NoLogo \"{}\" -r \"%V\"",
        vbs_shim_path.display()
    )
}

/// 生成隐藏的 dot.cmd 内容：供命令行终端输入 `dot` 时转发所有参数给同目录下的 dot.lnk 快捷方式
pub fn shim_script_content() -> &'static str {
    "@\"%~dp0dot.lnk\" %*\r\n"
}

/// 生成隐藏的 dot.vbs 内容：供右键“自动解密”以 WindowStyle=0 (SW_HIDE) 完全无弹窗静默调用同目录下的 dot.lnk
pub fn vbs_shim_script_content() -> &'static str {
    concat!(
        "Option Explicit\r\n",
        "Dim fso, dir, lnk, i, arg, args, sh, cmd, code, workDir\r\n",
        "Set fso = CreateObject(\"Scripting.FileSystemObject\")\r\n",
        "dir = fso.GetParentFolderName(WScript.ScriptFullName)\r\n",
        "lnk = fso.BuildPath(dir, \"dot.lnk\")\r\n",
        "args = \"\"\r\n",
        "workDir = \"\"\r\n",
        "For i = 0 To WScript.Arguments.Count - 1\r\n",
        "    arg = WScript.Arguments(i)\r\n",
        "    If Right(arg, 1) = \"\\\" Then\r\n",
        "        arg = arg & \".\"\r\n",
        "    End If\r\n",
        "    If workDir = \"\" And Left(arg, 1) <> \"-\" Then\r\n",
        "        If fso.FolderExists(arg) Then\r\n",
        "            workDir = arg\r\n",
        "        ElseIf fso.FileExists(arg) Then\r\n",
        "            workDir = fso.GetParentFolderName(arg)\r\n",
        "        End If\r\n",
        "    End If\r\n",
        "    args = args & \" \"\"\" & Replace(arg, \"\"\"\", \"\"\"\"\"\") & \"\"\"\"\r\n",
        "Next\r\n",
        "Set sh = CreateObject(\"WScript.Shell\")\r\n",
        "If workDir <> \"\" Then\r\n",
        "    On Error Resume Next\r\n",
        "    sh.CurrentDirectory = workDir\r\n",
        "    On Error GoTo 0\r\n",
        "End If\r\n",
        "cmd = \"cmd.exe /d /s /c \"\"\"\"\" & lnk & \"\"\" -q\" & args & \"\"\"\"\r\n",
        "code = sh.Run(cmd, 0, True)\r\n",
        "WScript.Quit code\r\n",
    )
}

/// 在 PATH 字符串中追加目录（若已存在则不重复添加，忽略大小写及末尾斜杠）
pub fn add_dir_to_path_string(existing_path: &str, dir_to_add: &Path) -> (String, bool) {
    let target = normalize_path_entry(&dir_to_add.to_string_lossy());
    if target.is_empty() {
        return (existing_path.to_string(), false);
    }

    let already_exists = existing_path
        .split(';')
        .map(normalize_path_entry)
        .any(|entry| !entry.is_empty() && entry.eq_ignore_ascii_case(&target));

    if already_exists {
        return (existing_path.to_string(), false);
    }

    let added = dir_to_add.to_string_lossy();
    let trimmed = existing_path.trim_end_matches(';');
    let updated = if trimmed.is_empty() {
        added.into_owned()
    } else {
        format!("{trimmed};{added}")
    };
    (updated, true)
}

/// 从 PATH 字符串中移除指定目录（忽略大小写、引号及末尾斜杠）
pub fn remove_dir_from_path_string(existing_path: &str, dir_to_remove: &Path) -> (String, bool) {
    let target = normalize_path_entry(&dir_to_remove.to_string_lossy());
    if target.is_empty() {
        return (existing_path.to_string(), false);
    }

    let mut kept = Vec::new();
    let mut removed = false;
    for part in existing_path.split(';') {
        let norm = normalize_path_entry(part);
        if !norm.is_empty() && norm.eq_ignore_ascii_case(&target) {
            removed = true;
        } else if !part.is_empty() {
            kept.push(part);
        }
    }
    (kept.join(";"), removed)
}

/// 确保 PATHEXT 中包含 .LNK，以便 cmd.exe 可直接从 PATH 解析 dot.lnk
pub fn ensure_lnk_in_pathext(existing_pathext: &str) -> (String, bool) {
    let base = if existing_pathext.trim().is_empty() {
        ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC"
    } else {
        existing_pathext.trim_end_matches(';')
    };

    let has_lnk = base
        .split(';')
        .any(|ext| ext.trim().eq_ignore_ascii_case(".LNK"));
    if has_lnk {
        (base.to_string(), false)
    } else {
        (format!("{base};.LNK"), true)
    }
}

fn normalize_path_entry(entry: &str) -> String {
    entry
        .trim()
        .trim_matches('"')
        .trim()
        .trim_end_matches(['\\', '/'])
        .to_string()
}

#[cfg(windows)]
pub fn install(
    cfg: &InstallConfig,
    python_exe_bytes: &[u8],
    uninstall_exe_bytes: &[u8],
) -> Result<()> {
    install_with_progress(cfg, python_exe_bytes, uninstall_exe_bytes, |_, _| {})
}

/// 执行完整安装流程，支持向 GUI 向导实时回调安装进度 (0..=100) 与状态日志
#[cfg(windows)]
pub fn install_with_progress<F>(
    cfg: &InstallConfig,
    python_exe_bytes: &[u8],
    uninstall_exe_bytes: &[u8],
    mut on_progress: F,
) -> Result<()>
where
    F: FnMut(u32, &str),
{
    on_progress(
        10,
        &format!("正在创建安装目录: {}", cfg.install_dir.display()),
    );
    fs::create_dir_all(&cfg.install_dir)
        .with_context(|| format!("创建安装目录失败: {}", cfg.install_dir.display()))?;

    let py_path = cfg.python_exe_path();
    on_progress(25, &format!("正在释放核心程序: {}", py_path.display()));
    write_executable(&py_path, python_exe_bytes)
        .with_context(|| format!("写入核心程序失败: {}", py_path.display()))?;

    let uninst_path = cfg.uninstall_exe_path();
    on_progress(40, &format!("正在释放卸载程序: {}", uninst_path.display()));
    write_executable(&uninst_path, uninstall_exe_bytes)
        .with_context(|| format!("写入卸载程序失败: {}", uninst_path.display()))?;

    on_progress(
        55,
        &format!("正在创建开始菜单目录: {}", cfg.start_menu_dir.display()),
    );
    fs::create_dir_all(&cfg.start_menu_dir)
        .with_context(|| format!("创建开始菜单目录失败: {}", cfg.start_menu_dir.display()))?;

    let ico_path = cfg.icon_path();
    let ico_bytes = crate::icon::generate_unlocked_padlock_ico();
    write_hidden_text_file(&ico_path, &ico_bytes)
        .with_context(|| format!("写入解锁图标文件失败: {}", ico_path.display()))?;

    let shortcut = cfg.shortcut_path();
    on_progress(65, &format!("正在创建开始菜单快捷方式: {}", shortcut.display()));
    create_shortcut(&shortcut, &py_path, &ico_path, "自动解密工具 (dot)")
        .with_context(|| format!("创建开始菜单快捷方式失败: {}", shortcut.display()))?;

    let shim = cfg.cmd_shim_path();
    write_hidden_text_file(&shim, shim_script_content().as_bytes())
        .with_context(|| format!("写入命令行入口脚本失败: {}", shim.display()))?;

    let vbs_shim = cfg.vbs_shim_path();
    write_hidden_text_file(&vbs_shim, vbs_shim_script_content().as_bytes())
        .with_context(|| format!("写入右键静默启动脚本失败: {}", vbs_shim.display()))?;

    if cfg.enable_path_env {
        on_progress(
            78,
            &format!("正在将快捷方式目录注入系统 PATH: {}", cfg.start_menu_dir.display()),
        );
        register_path_environment(cfg).context("注入 PATH 环境变量失败")?;
    }

    if cfg.enable_context_menu {
        on_progress(88, "正在注册资源管理器右键菜单: 自动解密（无弹窗静默模式）");
        register_context_menu(cfg).context("注入右键菜单“自动解密”失败")?;
    }

    on_progress(95, "正在写入 Windows 应用卸载注册表项...");
    register_uninstaller(cfg).context("写入系统卸载注册表项失败")?;

    if cfg.broadcast_changes {
        on_progress(98, "正在通知系统刷新环境变量与资源管理器...");
        broadcast_environment_change();
    }

    on_progress(100, "安装完成！");
    Ok(())
}

/// 执行完整卸载流程
#[cfg(windows)]
pub fn uninstall(cfg: &InstallConfig) -> Result<()> {
    unregister_context_menu(cfg).context("移除右键菜单注册表项失败")?;
    unregister_path_environment(cfg).context("从 PATH 移除快捷方式目录失败")?;
    unregister_uninstaller(cfg).context("移除卸载注册表项失败")?;

    if cfg.start_menu_dir.exists() {
        for hidden_file in [
            cfg.cmd_shim_path(),
            cfg.vbs_shim_path(),
            cfg.icon_path(),
            cfg.shortcut_path(),
        ] {
            if hidden_file.exists() {
                let _ = win::clear_hidden_and_readonly(&hidden_file);
                let _ = fs::remove_file(&hidden_file);
            }
        }
        let _ = fs::remove_dir_all(&cfg.start_menu_dir);
    }

    let py_path = cfg.python_exe_path();
    if py_path.exists() {
        let _ = win::clear_hidden_and_readonly(&py_path);
        fs::remove_file(&py_path)
            .with_context(|| format!("删除核心程序失败: {}", py_path.display()))?;
    }

    let uninst_path = cfg.uninstall_exe_path();
    if uninst_path.exists() {
        let _ = win::clear_hidden_and_readonly(&uninst_path);
        if fs::remove_file(&uninst_path).is_err() {
            schedule_self_delete(&uninst_path, &cfg.install_dir)?;
        } else {
            let _ = fs::remove_dir(&cfg.install_dir);
        }
    } else if cfg.install_dir.exists() {
        let _ = fs::remove_dir(&cfg.install_dir);
    }

    if cfg.broadcast_changes {
        broadcast_environment_change();
    }

    Ok(())
}

#[cfg(windows)]
fn write_executable(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let _ = win::clear_hidden_and_readonly(path);
    }
    fs::write(path, bytes)?;
    Ok(())
}

#[cfg(windows)]
fn write_hidden_text_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.exists() {
        let _ = win::clear_hidden_and_readonly(path);
        let _ = fs::remove_file(path);
    }
    fs::write(path, bytes)?;
    win::set_file_hidden(path)?;
    Ok(())
}

#[cfg(windows)]
pub fn create_shortcut(
    shortcut_path: &Path,
    target_exe: &Path,
    icon_path: &Path,
    description: &str,
) -> Result<()> {
    if let Some(parent) = shortcut_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if shortcut_path.exists() {
        let _ = win::clear_hidden_and_readonly(shortcut_path);
        let _ = fs::remove_file(shortcut_path);
    }
    win::create_shell_link(shortcut_path, target_exe, icon_path, description)
}

#[cfg(windows)]
fn register_path_environment(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    let key = win::reg_create_key(hive, &cfg.env_subkey)?;

    let (existing_path, value_type) = win::reg_query_string(key.0, "Path")?
        .unwrap_or_else(|| (String::new(), win::REG_EXPAND_SZ));
    let (new_path, changed) = add_dir_to_path_string(&existing_path, &cfg.start_menu_dir);
    if changed || existing_path.is_empty() {
        let ty = if value_type == win::REG_SZ {
            win::REG_EXPAND_SZ
        } else {
            value_type
        };
        win::reg_set_string(key.0, "Path", &new_path, ty)?;
    }

    let (existing_ext, ext_type) = win::reg_query_string(key.0, "PATHEXT")?
        .unwrap_or_else(|| (String::new(), win::REG_SZ));
    let (new_ext, ext_changed) = ensure_lnk_in_pathext(&existing_ext);
    if ext_changed {
        win::reg_set_string(key.0, "PATHEXT", &new_ext, ext_type)?;
    }

    Ok(())
}

#[cfg(windows)]
fn unregister_path_environment(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    let Some(key) = win::reg_open_key_Optional(hive, &cfg.env_subkey)? else {
        return Ok(());
    };

    if let Some((existing_path, value_type)) = win::reg_query_string(key.0, "Path")? {
        let (new_path, changed) = remove_dir_from_path_string(&existing_path, &cfg.start_menu_dir);
        if changed {
            win::reg_set_string(key.0, "Path", &new_path, value_type)?;
        }
    }
    Ok(())
}

#[cfg(windows)]
fn register_context_menu(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    let vbs_shim = cfg.vbs_shim_path();
    let icon_val = format!("{},0", cfg.icon_path().display());
    let file_cmd = file_context_command(&vbs_shim);
    let dir_cmd = directory_context_command(&vbs_shim);

    let targets = [
        (
            format!(r"{}\*\shell\{}", cfg.classes_subkey, CONTEXT_MENU_KEY_NAME),
            &file_cmd,
        ),
        (
            format!(
                r"{}\Directory\shell\{}",
                cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
            ),
            &dir_cmd,
        ),
        (
            format!(
                r"{}\Directory\Background\shell\{}",
                cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
            ),
            &dir_cmd,
        ),
    ];

    for (menu_subkey, cmd_str) in targets {
        let menu_key = win::reg_create_key(hive, &menu_subkey)?;
        win::reg_set_string(menu_key.0, "", CONTEXT_MENU_LABEL, win::REG_SZ)?;
        win::reg_set_string(menu_key.0, "MUIVerb", CONTEXT_MENU_LABEL, win::REG_SZ)?;
        win::reg_set_string(menu_key.0, "Icon", &icon_val, win::REG_SZ)?;

        let cmd_subkey = format!(r"{menu_subkey}\command");
        let cmd_key = win::reg_create_key(hive, &cmd_subkey)?;
        win::reg_set_string(cmd_key.0, "", cmd_str, win::REG_SZ)?;
    }

    Ok(())
}

#[cfg(windows)]
fn unregister_context_menu(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    let keys = [
        format!(r"{}\*\shell\{}", cfg.classes_subkey, CONTEXT_MENU_KEY_NAME),
        format!(
            r"{}\Directory\shell\{}",
            cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
        ),
        format!(
            r"{}\Directory\Background\shell\{}",
            cfg.classes_subkey, CONTEXT_MENU_KEY_NAME
        ),
    ];

    for subkey in keys {
        win::reg_delete_tree_ignore_missing(hive, &subkey)?;
    }
    Ok(())
}

#[cfg(windows)]
fn register_uninstaller(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    let key = win::reg_create_key(hive, &cfg.uninstall_subkey)?;
    let uninst_exe = cfg.uninstall_exe_path();
    let uninst_cmd = format!("\"{}\"", uninst_exe.display());
    let quiet_cmd = format!("\"{}\" --quiet", uninst_exe.display());
    let display_icon = format!("{},0", cfg.icon_path().display());

    win::reg_set_string(key.0, "DisplayName", "Dot (自动解密工具)", win::REG_SZ)?;
    win::reg_set_string(
        key.0,
        "DisplayVersion",
        env!("CARGO_PKG_VERSION"),
        win::REG_SZ,
    )?;
    win::reg_set_string(key.0, "Publisher", "Goodnice", win::REG_SZ)?;
    win::reg_set_string(
        key.0,
        "InstallLocation",
        &cfg.install_dir.to_string_lossy(),
        win::REG_SZ,
    )?;
    win::reg_set_string(key.0, "DisplayIcon", &display_icon, win::REG_SZ)?;
    win::reg_set_string(key.0, "UninstallString", &uninst_cmd, win::REG_SZ)?;
    win::reg_set_string(key.0, "QuietUninstallString", &quiet_cmd, win::REG_SZ)?;
    win::reg_set_dword(key.0, "NoModify", 1)?;
    win::reg_set_dword(key.0, "NoRepair", 1)?;

    Ok(())
}

#[cfg(windows)]
fn unregister_uninstaller(cfg: &InstallConfig) -> Result<()> {
    let hive = cfg.registry_hive.raw_handle();
    win::reg_delete_tree_ignore_missing(hive, &cfg.uninstall_subkey)
}

#[cfg(windows)]
fn schedule_self_delete(uninst_exe: &Path, install_dir: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let cmd_line = format!(
        "ping 127.0.0.1 -n 2 > nul & del /f /q \"{}\" > nul 2>&1 & rmdir \"{}\" > nul 2>&1",
        uninst_exe.display(),
        install_dir.display()
    );
    Command::new("cmd.exe")
        .args(["/d", "/c", &cmd_line])
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("无法启动延迟自清理进程")?;
    Ok(())
}

#[cfg(windows)]
pub fn is_elevated() -> bool {
    win::is_user_an_admin()
}

#[cfg(windows)]
pub fn relaunch_as_admin() -> Result<u32> {
    let exe = env::current_exe().context("无法获取当前程序路径")?;
    let args: Vec<String> = env::args().skip(1).map(|a| quote_arg(&a)).collect();
    let params = args.join(" ");
    win::run_elevated(&exe, &params)
}

fn quote_arg(arg: &str) -> String {
    if arg.is_empty() || arg.contains([' ', '\t', '"']) {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}

#[cfg(windows)]
pub fn broadcast_environment_change() {
    win::broadcast_setting_change();
}

#[cfg(windows)]
pub fn enable_utf8_console() {
    win::enable_utf8_console();
}

#[cfg(windows)]
pub mod win {
    use super::*;

    pub const HKEY_CURRENT_USER: isize = -2147483647i32 as isize; // 0x80000001
    pub const HKEY_LOCAL_MACHINE: isize = -2147483646i32 as isize; // 0x80000002

    pub const KEY_READ: u32 = 0x20019;
    pub const KEY_WRITE: u32 = 0x20006;
    pub const KEY_ALL_ACCESS: u32 = 0xF003F;
    pub const KEY_WOW64_64KEY: u32 = 0x0100;

    pub const REG_SZ: u32 = 1;
    pub const REG_EXPAND_SZ: u32 = 2;
    pub const REG_DWORD: u32 = 4;

    pub const ERROR_SUCCESS: i32 = 0;
    pub const ERROR_FILE_NOT_FOUND: i32 = 2;
    pub const ERROR_PATH_NOT_FOUND: i32 = 3;

    const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x0000_0002;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const INVALID_FILE_ATTRIBUTES: u32 = 0xFFFF_FFFF;

    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SW_SHOWNORMAL: i32 = 1;
    const INFINITE: u32 = 0xFFFF_FFFF;

    const HWND_BROADCAST: isize = 0xFFFF;
    const WM_SETTINGCHANGE: u32 = 0x001A;
    const SMTO_ABORTIFHUNG: u32 = 0x0002;

    const SHCNE_ASSOCCHANGED: i32 = 0x0800_0000;
    const SHCNF_IDLIST: u32 = 0x0000;

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;

    pub const MB_OK: u32 = 0x0000_0000;
    pub const MB_YESNO: u32 = 0x0000_0004;
    pub const MB_ICONHAND: u32 = 0x0000_0010;
    pub const MB_ICONQUESTION: u32 = 0x0000_0020;
    pub const MB_ICONINFORMATION: u32 = 0x0000_0040;
    pub const IDYES: i32 = 6;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct GUID {
        pub data1: u32,
        pub data2: u16,
        pub data3: u16,
        pub data4: [u8; 8],
    }

    const CLSID_SHELL_LINK: GUID = GUID {
        data1: 0x00021401,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    const IID_ISHELL_LINK_W: GUID = GUID {
        data1: 0x000214F9,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    const IID_IPERSIST_FILE: GUID = GUID {
        data1: 0x0000010B,
        data2: 0x0000,
        data3: 0x0000,
        data4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    #[repr(C)]
    struct IShellLinkWVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_path: *const c_void,
        get_id_list: *const c_void,
        set_id_list: *const c_void,
        get_description: *const c_void,
        set_description: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
        get_working_directory: *const c_void,
        set_working_directory: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
        get_arguments: *const c_void,
        set_arguments: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
        get_hotkey: *const c_void,
        set_hotkey: *const c_void,
        get_show_cmd: *const c_void,
        set_show_cmd: *const c_void,
        get_icon_location: *const c_void,
        set_icon_location: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
        set_relative_path: *const c_void,
        resolve: *const c_void,
        set_path: unsafe extern "system" fn(*mut c_void, *const u16) -> i32,
    }

    #[repr(C)]
    struct IShellLinkW {
        lp_vtbl: *const IShellLinkWVtbl,
    }

    #[repr(C)]
    struct IPersistFileVtbl {
        query_interface:
            unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> i32,
        add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        get_class_id: *const c_void,
        is_dirty: *const c_void,
        load: *const c_void,
        save: unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32,
        save_completed: *const c_void,
        get_cur_file: *const c_void,
    }

    #[repr(C)]
    struct IPersistFile {
        lp_vtbl: *const IPersistFileVtbl,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct SHELLEXECUTEINFOW {
        cb_size: u32,
        f_mask: u32,
        hwnd: isize,
        lp_verb: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show: i32,
        h_inst_app: isize,
        lp_id_list: *mut c_void,
        lp_class: *const u16,
        hkey_class: isize,
        dw_hot_key: u32,
        h_icon_or_monitor: isize,
        h_process: *mut c_void,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegCreateKeyExW(
            hKey: isize,
            lpSubKey: *const u16,
            Reserved: u32,
            lpClass: *const u16,
            dwOptions: u32,
            samDesired: u32,
            lpSecurityAttributes: *const c_void,
            phkResult: *mut isize,
            lpdwDisposition: *mut u32,
        ) -> i32;
        fn RegOpenKeyExW(
            hKey: isize,
            lpSubKey: *const u16,
            ulOptions: u32,
            samDesired: u32,
            phkResult: *mut isize,
        ) -> i32;
        fn RegQueryValueExW(
            hKey: isize,
            lpValueName: *const u16,
            lpReserved: *const u32,
            lpType: *mut u32,
            lpData: *mut u8,
            lpcbData: *mut u32,
        ) -> i32;
        fn RegSetValueExW(
            hKey: isize,
            lpValueName: *const u16,
            Reserved: u32,
            dwType: u32,
            lpData: *const u8,
            cbData: u32,
        ) -> i32;
        fn RegDeleteTreeW(hKey: isize, lpSubKey: *const u16) -> i32;
        fn RegCloseKey(hKey: isize) -> i32;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
        fn ShellExecuteExW(pExecInfo: *mut SHELLEXECUTEINFOW) -> i32;
        fn SHChangeNotify(
            wEventId: i32,
            uFlags: u32,
            dwItem1: *const c_void,
            dwItem2: *const c_void,
        );
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetFileAttributesW(lpFileName: *const u16, dwFileAttributes: u32) -> i32;
        fn GetFileAttributesW(lpFileName: *const u16) -> u32;
        fn WaitForSingleObject(hHandle: *mut c_void, dwMilliseconds: u32) -> u32;
        fn GetExitCodeProcess(hProcess: *mut c_void, lpExitCode: *mut u32) -> i32;
        fn CloseHandle(hObject: *mut c_void) -> i32;
        fn SetConsoleOutputCP(wCodePageID: u32) -> i32;
        fn SetConsoleCP(wCodePageID: u32) -> i32;
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn SendMessageTimeoutW(
            hWnd: isize,
            Msg: u32,
            wParam: usize,
            lParam: *const u16,
            fuFlags: u32,
            uTimeout: u32,
            lpdwResult: *mut usize,
        ) -> isize;
        pub fn MessageBoxW(
            hWnd: isize,
            lpText: *const u16,
            lpCaption: *const u16,
            uType: u32,
        ) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        pub fn CoInitializeEx(pvReserved: *const c_void, dwCoInit: u32) -> i32;
        fn CoCreateInstance(
            rclsid: *const GUID,
            pUnkOuter: *mut c_void,
            dwClsContext: u32,
            riid: *const GUID,
            ppv: *mut *mut c_void,
        ) -> i32;
        pub fn CoUninitialize();
    }

    pub struct RegKeyGuard(pub isize);

    impl Drop for RegKeyGuard {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe {
                    RegCloseKey(self.0);
                }
            }
        }
    }

    pub fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn path_to_wide(p: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        p.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn message_box(hwnd: isize, text: &str, caption: &str, flags: u32) -> i32 {
        let t = to_wide(text);
        let c = to_wide(caption);
        unsafe { MessageBoxW(hwnd, t.as_ptr(), c.as_ptr(), flags) }
    }

    pub fn enable_utf8_console() {
        const UTF8: u32 = 65001;
        unsafe {
            SetConsoleOutputCP(UTF8);
            SetConsoleCP(UTF8);
        }
    }

    pub fn is_user_an_admin() -> bool {
        unsafe { IsUserAnAdmin() != 0 }
    }

    pub fn run_elevated(exe: &Path, params: &str) -> Result<u32> {
        let verb = to_wide("runas");
        let file = path_to_wide(exe);
        let parameters = to_wide(params);

        let mut sei = SHELLEXECUTEINFOW {
            cb_size: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            f_mask: SEE_MASK_NOCLOSEPROCESS,
            hwnd: 0,
            lp_verb: verb.as_ptr(),
            lp_file: file.as_ptr(),
            lp_parameters: parameters.as_ptr(),
            lp_directory: null(),
            n_show: SW_SHOWNORMAL,
            h_inst_app: 0,
            lp_id_list: null_mut(),
            lp_class: null(),
            hkey_class: 0,
            dw_hot_key: 0,
            h_icon_or_monitor: isize::default(),
            h_process: null_mut(),
        };

        let ok = unsafe { ShellExecuteExW(&mut sei) };
        if ok == 0 {
            let err = io::Error::last_os_error();
            bail!("请求管理员权限 (UAC) 失败或被用户取消: {err}");
        }

        let mut exit_code: u32 = 0;
        if !sei.h_process.is_null() {
            unsafe {
                WaitForSingleObject(sei.h_process, INFINITE);
                GetExitCodeProcess(sei.h_process, &mut exit_code);
                CloseHandle(sei.h_process);
            }
        }
        Ok(exit_code)
    }

    pub fn set_file_hidden(path: &Path) -> Result<()> {
        let wide = path_to_wide(path);
        let current = unsafe { GetFileAttributesW(wide.as_ptr()) };
        let attrs = if current == INVALID_FILE_ATTRIBUTES {
            FILE_ATTRIBUTE_HIDDEN
        } else {
            current | FILE_ATTRIBUTE_HIDDEN
        };
        let ok = unsafe { SetFileAttributesW(wide.as_ptr(), attrs) };
        if ok == 0 {
            return Err(io::Error::last_os_error())
                .with_context(|| format!("设置隐藏属性失败: {}", path.display()));
        }
        Ok(())
    }

    pub fn clear_hidden_and_readonly(path: &Path) -> Result<()> {
        let wide = path_to_wide(path);
        let current = unsafe { GetFileAttributesW(wide.as_ptr()) };
        if current == INVALID_FILE_ATTRIBUTES {
            return Ok(());
        }
        let cleared = current & !(FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_READONLY);
        let new_attrs = if cleared == 0 {
            FILE_ATTRIBUTE_NORMAL
        } else {
            cleared
        };
        unsafe {
            SetFileAttributesW(wide.as_ptr(), new_attrs);
        }
        Ok(())
    }

    pub fn is_file_hidden(path: &Path) -> bool {
        let wide = path_to_wide(path);
        let attrs = unsafe { GetFileAttributesW(wide.as_ptr()) };
        attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_HIDDEN) != 0
    }

    pub fn create_shell_link(
        shortcut_path: &Path,
        target_exe: &Path,
        icon_path: &Path,
        description: &str,
    ) -> Result<()> {
        unsafe {
            let hr_init = CoInitializeEx(null(), COINIT_APARTMENTTHREADED);
            let should_uninit = hr_init >= 0;

            let mut shell_link_ptr: *mut c_void = null_mut();
            let hr = CoCreateInstance(
                &CLSID_SHELL_LINK,
                null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut shell_link_ptr,
            );
            if hr < 0 || shell_link_ptr.is_null() {
                if should_uninit {
                    CoUninitialize();
                }
                bail!("创建 IShellLinkW COM 实例失败: HRESULT 0x{hr:08X}");
            }

            let sl = shell_link_ptr as *mut IShellLinkW;
            let vtbl = &*(*sl).lp_vtbl;

            let target_w = path_to_wide(target_exe);
            let icon_w = path_to_wide(icon_path);
            let desc_w = to_wide(description);
            let empty_w = to_wide("");

            let hr_path = (vtbl.set_path)(shell_link_ptr, target_w.as_ptr());
            let _ = (vtbl.set_description)(shell_link_ptr, desc_w.as_ptr());
            let _ = (vtbl.set_working_directory)(shell_link_ptr, empty_w.as_ptr());
            let _ = (vtbl.set_icon_location)(shell_link_ptr, icon_w.as_ptr(), 0);

            if hr_path < 0 {
                (vtbl.release)(shell_link_ptr);
                if should_uninit {
                    CoUninitialize();
                }
                bail!("IShellLinkW::SetPath 失败: HRESULT 0x{hr_path:08X}");
            }

            let mut persist_ptr: *mut c_void = null_mut();
            let hr_qi = (vtbl.query_interface)(
                shell_link_ptr,
                &IID_IPERSIST_FILE,
                &mut persist_ptr,
            );
            if hr_qi < 0 || persist_ptr.is_null() {
                (vtbl.release)(shell_link_ptr);
                if should_uninit {
                    CoUninitialize();
                }
                bail!("QueryInterface(IPersistFile) 失败: HRESULT 0x{hr_qi:08X}");
            }

            let pf = persist_ptr as *mut IPersistFile;
            let pf_vtbl = &*(*pf).lp_vtbl;
            let link_w = path_to_wide(shortcut_path);
            let hr_save = (pf_vtbl.save)(persist_ptr, link_w.as_ptr(), 1);

            (pf_vtbl.release)(persist_ptr);
            (vtbl.release)(shell_link_ptr);
            if should_uninit {
                CoUninitialize();
            }

            if hr_save < 0 {
                bail!(
                    "保存快捷方式 {} 失败: HRESULT 0x{hr_save:08X}",
                    shortcut_path.display()
                );
            }
        }
        Ok(())
    }

    pub fn reg_create_key(hive: isize, subkey: &str) -> Result<RegKeyGuard> {
        let wide = to_wide(subkey);
        let mut hkey: isize = 0;
        let mut disposition: u32 = 0;
        let status = unsafe {
            RegCreateKeyExW(
                hive,
                wide.as_ptr(),
                0,
                null(),
                0,
                KEY_ALL_ACCESS | KEY_WOW64_64KEY,
                null(),
                &mut hkey,
                &mut disposition,
            )
        };
        if status != ERROR_SUCCESS {
            bail!(
                "无法创建或打开注册表项 {subkey}: Win32 错误码 {status} (请确认是否具有管理员权限)"
            );
        }
        Ok(RegKeyGuard(hkey))
    }

    #[allow(non_snake_case)]
    pub fn reg_open_key_Optional(hive: isize, subkey: &str) -> Result<Option<RegKeyGuard>> {
        let wide = to_wide(subkey);
        let mut hkey: isize = 0;
        let status = unsafe {
            RegOpenKeyExW(
                hive,
                wide.as_ptr(),
                0,
                KEY_READ | KEY_WRITE | KEY_WOW64_64KEY,
                &mut hkey,
            )
        };
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            bail!("无法打开注册表项 {subkey}: Win32 错误码 {status}");
        }
        Ok(Some(RegKeyGuard(hkey)))
    }

    pub fn reg_query_string(hkey: isize, name: &str) -> Result<Option<(String, u32)>> {
        let wide_name = to_wide(name);
        let mut value_type: u32 = 0;
        let mut byte_len: u32 = 0;
        let status = unsafe {
            RegQueryValueExW(
                hkey,
                wide_name.as_ptr(),
                null(),
                &mut value_type,
                null_mut(),
                &mut byte_len,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            bail!("读取注册表值 {name} 长度失败: Win32 错误码 {status}");
        }
        if byte_len == 0 {
            return Ok(Some((String::new(), value_type)));
        }

        let u16_len = (byte_len as usize).div_ceil(2);
        let mut buf = vec![0u16; u16_len];
        let mut actual_bytes = (u16_len * 2) as u32;
        let status = unsafe {
            RegQueryValueExW(
                hkey,
                wide_name.as_ptr(),
                null(),
                &mut value_type,
                buf.as_mut_ptr() as *mut u8,
                &mut actual_bytes,
            )
        };
        if status != ERROR_SUCCESS {
            bail!("读取注册表值 {name} 内容失败: Win32 错误码 {status}");
        }

        let valid_u16 = (actual_bytes as usize) / 2;
        buf.truncate(valid_u16);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        Ok(Some((String::from_utf16_lossy(&buf), value_type)))
    }

    pub fn reg_set_string(hkey: isize, name: &str, value: &str, value_type: u32) -> Result<()> {
        let wide_name = to_wide(name);
        let wide_val = to_wide(value);
        let byte_len = (wide_val.len() * 2) as u32;
        let status = unsafe {
            RegSetValueExW(
                hkey,
                wide_name.as_ptr(),
                0,
                value_type,
                wide_val.as_ptr() as *const u8,
                byte_len,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(anyhow!(
                "写入注册表值 {name} 失败: Win32 错误码 {status}"
            ));
        }
        Ok(())
    }

    pub fn reg_set_dword(hkey: isize, name: &str, value: u32) -> Result<()> {
        let wide_name = to_wide(name);
        let bytes = value.to_le_bytes();
        let status = unsafe {
            RegSetValueExW(
                hkey,
                wide_name.as_ptr(),
                0,
                REG_DWORD,
                bytes.as_ptr(),
                4,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(anyhow!(
                "写入注册表 DWORD 值 {name} 失败: Win32 错误码 {status}"
            ));
        }
        Ok(())
    }

    pub fn reg_delete_tree_ignore_missing(hive: isize, subkey: &str) -> Result<()> {
        let wide = to_wide(subkey);
        let status = unsafe { RegDeleteTreeW(hive, wide.as_ptr()) };
        if status == ERROR_SUCCESS
            || status == ERROR_FILE_NOT_FOUND
            || status == ERROR_PATH_NOT_FOUND
        {
            return Ok(());
        }
        bail!("删除注册表树 {subkey} 失败: Win32 错误码 {status}");
    }

    pub fn broadcast_setting_change() {
        let env_wide = to_wide("Environment");
        let mut result: usize = 0;
        unsafe {
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env_wide.as_ptr(),
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
            SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, null(), null());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_and_remove_path_entries_preserve_existing_and_ignore_case() {
        let start_menu = Path::new(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot");
        let initial = r"C:\Windows\system32;C:\Windows";

        let (added, changed) = add_dir_to_path_string(initial, start_menu);
        assert!(changed);
        assert_eq!(
            added,
            r"C:\Windows\system32;C:\Windows;C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot"
        );

        let (dup, changed2) = add_dir_to_path_string(
            &format!("{added}\\"),
            Path::new(r"c:\programdata\microsoft\windows\start menu\programs\dot"),
        );
        assert!(!changed2);
        assert!(dup.starts_with(&added));

        let (removed, was_removed) = remove_dir_from_path_string(&added, start_menu);
        assert!(was_removed);
        assert_eq!(removed, initial);
    }

    #[test]
    fn pathext_ensures_lnk_once() {
        let (updated, changed) = ensure_lnk_in_pathext(".COM;.EXE;.BAT;.CMD");
        assert!(changed);
        assert_eq!(updated, ".COM;.EXE;.BAT;.CMD;.LNK");

        let (again, changed2) = ensure_lnk_in_pathext(&updated);
        assert!(!changed2);
        assert_eq!(again, ".COM;.EXE;.BAT;.CMD;.LNK");
    }

    #[test]
    fn context_commands_use_windowless_vbs_and_never_expose_python_exe() {
        let vbs_shim =
            Path::new(r"C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot\dot.vbs");
        let file_cmd = file_context_command(vbs_shim);
        let dir_cmd = directory_context_command(vbs_shim);

        assert!(file_cmd.starts_with("wscript.exe //B //NoLogo"));
        assert!(file_cmd.contains("dot.vbs"));
        assert!(!file_cmd.to_ascii_lowercase().contains("python.exe"));
        assert!(file_cmd.contains("\"%1\""));

        assert!(dir_cmd.starts_with("wscript.exe //B //NoLogo"));
        assert!(dir_cmd.contains("dot.vbs"));
        assert!(!dir_cmd.to_ascii_lowercase().contains("python.exe"));
        assert!(dir_cmd.contains("-r"));
        assert!(dir_cmd.contains("\"%V\""));

        assert!(vbs_shim_script_content().contains("dot.lnk"));
        assert!(vbs_shim_script_content().contains("sh.Run(cmd, 0, True)"));
        assert!(!vbs_shim_script_content().to_ascii_lowercase().contains("python.exe"));
    }
}
