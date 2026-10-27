#![windows_subsystem = "windows"]

use std::env;
use std::ffi::c_void;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::ptr::null;

use anyhow::{Context, Result, bail};
use clap::Parser;

use goodnice::installer::{
    InstallConfig, PYTHON_EXE_NAME, UNINSTALL_EXE_NAME, install_with_progress, is_elevated,
    relaunch_as_admin, win,
};

include!(concat!(env!("OUT_DIR"), "/embedded_payload.rs"));

#[derive(Parser, Debug)]
#[command(
    name = "setup",
    version,
    about = "Dot (自动解密工具) Windows 安装向导"
)]
struct SetupArgs {
    /// 自定义安装目录（默认：C:\Program Files\Dot）
    #[arg(long, value_name = "DIR")]
    install_dir: Option<PathBuf>,

    /// 自定义开始菜单快捷方式目录（默认：C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot）
    #[arg(long, value_name = "DIR")]
    start_menu_dir: Option<PathBuf>,

    /// 静默无界面安装
    #[arg(short, long)]
    quiet: bool,

    /// 不自动触发 UAC 提权（供测试或已提权环境使用）
    #[arg(long)]
    no_elevate: bool,
}

fn main() -> ExitCode {
    let args = SetupArgs::parse();
    let quiet = args.quiet;

    match run(args) {
        Ok(code) => code,
        Err(err) => {
            #[cfg(windows)]
            if !quiet {
                win::message_box(
                    0,
                    &format!("安装过程中发生错误：\n\n{err:#}"),
                    "Dot (自动解密工具) 安装程序",
                    win::MB_OK | win::MB_ICONHAND,
                );
            }
            let _ = quiet;
            ExitCode::from(1)
        }
    }
}

#[cfg(windows)]
fn run(args: SetupArgs) -> Result<ExitCode> {
    if !args.no_elevate && !is_elevated() {
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

    let (py_bytes, uninst_bytes) = load_payload_binaries()?;

    if args.quiet {
        install_with_progress(&cfg, &py_bytes, &uninst_bytes, |_, _| {})?;
        return Ok(ExitCode::SUCCESS);
    }

    gui::run_wizard(cfg, py_bytes, uninst_bytes)
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
            "未找到内置或同目录的 {} (期望路径: {})。请使用 `.\\package.ps1` 构建完整安装包。",
            PYTHON_EXE_NAME,
            py_path.display()
        );
    }
    if !uninst_path.is_file() {
        bail!(
            "未找到内置或同目录的 {} (期望路径: {})。请使用 `.\\package.ps1` 构建完整安装包。",
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

#[cfg(windows)]
mod gui {
    use super::*;
    use win::to_wide;

    const IDC_BTN_BACK: usize = 1001;
    const IDC_BTN_NEXT: usize = 1002;
    const IDC_BTN_CANCEL: usize = 1003;
    const IDC_BTN_BROWSE: usize = 1004;
    const IDC_EDIT_DIR: usize = 1005;
    const IDC_CHK_STARTMENU: usize = 1006;
    const IDC_CHK_PATH: usize = 1007;
    const IDC_CHK_CONTEXT: usize = 1008;

    const WM_CREATE: u32 = 0x0001;
    const WM_DESTROY: u32 = 0x0002;
    const WM_CLOSE: u32 = 0x0010;
    const WM_PAINT: u32 = 0x000F;
    const WM_SETFONT: u32 = 0x0030;
    const WM_COMMAND: u32 = 0x0111;
    const WM_CTLCOLORSTATIC: u32 = 0x0138;
    const WM_APP_START_INSTALL: u32 = 0x8001;

    const WS_OVERLAPPED: u32 = 0x0000_0000;
    const WS_CAPTION: u32 = 0x00C0_0000;
    const WS_SYSMENU: u32 = 0x0008_0000;
    const WS_MINIMIZEBOX: u32 = 0x0002_0000;
    const WS_VISIBLE: u32 = 0x1000_0000;
    const WS_CHILD: u32 = 0x4000_0000;
    const WS_TABSTOP: u32 = 0x0001_0000;
    const WS_BORDER: u32 = 0x0080_0000;
    const WS_VSCROLL: u32 = 0x0020_0000;
    const WS_DISABLED: u32 = 0x0800_0000;

    const WS_EX_CLIENTEDGE: u32 = 0x0000_0200;

    const BS_DEFPUSHBUTTON: u32 = 0x0001;
    const BS_AUTOCHECKBOX: u32 = 0x0003;
    const BM_GETCHECK: u32 = 0x00F0;
    const BM_SETCHECK: u32 = 0x00F1;
    const BST_CHECKED: usize = 1;

    const ES_AUTOHSCROLL: u32 = 0x0080;
    const ES_MULTILINE: u32 = 0x0004;
    const ES_READONLY: u32 = 0x0800;

    const SS_ETCHEDHORZ: u32 = 0x0010;

    const PBM_SETRANGE32: u32 = 0x0406;
    const PBM_SETPOS: u32 = 0x0402;

    const SW_SHOW: i32 = 5;
    const SW_HIDE: i32 = 0;

    const COLOR_WINDOW: i32 = 5;
    const COLOR_BTNFACE: i32 = 15;
    const TRANSPARENT: i32 = 1;

    const IDC_ARROW: *const u16 = 32512 as *const u16;
    const IDI_APPLICATION: *const u16 = 32512 as *const u16;

    const BIF_RETURNONLYFSDIRS: u32 = 0x0001;
    const BIF_NEWDIALOGSTYLE: u32 = 0x0040;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum WizardPage {
        Welcome,
        Options,
        Installing,
        Finished,
    }

    struct WizardState {
        cfg: InstallConfig,
        py_bytes: Vec<u8>,
        uninst_bytes: Vec<u8>,
        page: WizardPage,
        scale: f32,
        font_normal: isize,
        font_title: isize,
        white_brush: isize,
        app_icon: isize,
        lbl_header_title: isize,
        lbl_header_sub: isize,
        // Page 0: Welcome
        lbl_welcome: isize,
        // Page 1: Options
        lbl_dir_desc: isize,
        edit_dir: isize,
        btn_browse: isize,
        lbl_tasks: isize,
        chk_startmenu: isize,
        chk_path: isize,
        chk_context: isize,
        // Page 2: Installing
        lbl_progress_status: isize,
        progress_bar: isize,
        edit_log: isize,
        log_text: String,
        // Page 3: Finished
        lbl_finish: isize,
        // Footer buttons
        btn_back: isize,
        btn_next: isize,
        btn_cancel: isize,
        install_succeeded: bool,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct WNDCLASSEXW {
        cb_size: u32,
        style: u32,
        lpfn_wnd_proc: unsafe extern "system" fn(isize, u32, usize, isize) -> isize,
        cb_cls_extra: i32,
        cb_wnd_extra: i32,
        h_instance: isize,
        h_icon: isize,
        h_cursor: isize,
        hbr_background: isize,
        lpsz_menu_name: *const u16,
        lpsz_class_name: *const u16,
        h_icon_sm: isize,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct POINT {
        x: i32,
        y: i32,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct MSG {
        hwnd: isize,
        message: u32,
        w_param: usize,
        l_param: isize,
        time: u32,
        pt: POINT,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct RECT {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct PAINTSTRUCT {
        hdc: isize,
        f_erase: i32,
        rc_paint: RECT,
        f_restore: i32,
        f_inc_update: i32,
        rgb_reserved: [u8; 32],
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct INITCOMMONCONTROLSEX {
        dw_size: u32,
        dw_icc: u32,
    }

    #[repr(C)]
    #[allow(clippy::upper_case_acronyms)]
    struct BROWSEINFOW {
        hwnd_owner: isize,
        pidl_root: *const c_void,
        psz_display_name: *mut u16,
        lpsz_title: *const u16,
        ul_flags: u32,
        lpfn: *const c_void,
        l_param: isize,
        i_image: i32,
    }

    #[link(name = "user32")]
    unsafe extern "system" {
        fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> u16;
        fn CreateWindowExW(
            dwExStyle: u32,
            lpClassName: *const u16,
            lpWindowName: *const u16,
            dwStyle: u32,
            X: i32,
            Y: i32,
            nWidth: i32,
            nHeight: i32,
            hWndParent: isize,
            hMenu: isize,
            hInstance: isize,
            lpParam: *const c_void,
        ) -> isize;
        fn ShowWindow(hWnd: isize, nCmdShow: i32) -> i32;
        fn UpdateWindow(hWnd: isize) -> i32;
        fn EnableWindow(hWnd: isize, bEnable: i32) -> i32;
        fn DestroyWindow(hWnd: isize) -> i32;
        fn PostQuitMessage(nExitCode: i32);
        fn DefWindowProcW(hWnd: isize, Msg: u32, wParam: usize, lParam: isize) -> isize;
        fn GetMessageW(lpMsg: *mut MSG, hWnd: isize, wMsgFilterMin: u32, wMsgFilterMax: u32)
        -> i32;
        fn PeekMessageW(
            lpMsg: *mut MSG,
            hWnd: isize,
            wMsgFilterMin: u32,
            wMsgFilterMax: u32,
            wRemoveMsg: u32,
        ) -> i32;
        fn TranslateMessage(lpMsg: *const MSG) -> i32;
        fn DispatchMessageW(lpMsg: *const MSG) -> isize;
        fn SendMessageW(hWnd: isize, Msg: u32, wParam: usize, lParam: isize) -> isize;
        fn PostMessageW(hWnd: isize, Msg: u32, wParam: usize, lParam: isize) -> i32;
        fn SetWindowTextW(hWnd: isize, lpString: *const u16) -> i32;
        fn GetWindowTextLengthW(hWnd: isize) -> i32;
        fn GetWindowTextW(hWnd: isize, lpString: *mut u16, nMaxCount: i32) -> i32;
        fn GetSystemMetrics(nIndex: i32) -> i32;
        fn LoadCursorW(hInstance: isize, lpCursorName: *const u16) -> isize;
        fn LoadIconW(hInstance: isize, lpIconName: *const u16) -> isize;
        fn BeginPaint(hWnd: isize, lpPaint: *mut PAINTSTRUCT) -> isize;
        fn EndPaint(hWnd: isize, lpPaint: *const PAINTSTRUCT) -> i32;
        fn FillRect(hDC: isize, lprc: *const RECT, hbr: isize) -> i32;
        fn DrawIconEx(
            hdc: isize,
            xLeft: i32,
            yTop: i32,
            hIcon: isize,
            cxWidth: i32,
            cyWidth: i32,
            istepIfAniCur: u32,
            hbrFlickerFreeDraw: isize,
            diFlags: u32,
        ) -> i32;
        fn GetSysColorBrush(nIndex: i32) -> isize;
        fn SetWindowLongPtrW(hWnd: isize, nIndex: i32, dwNewLong: isize) -> isize;
        fn GetWindowLongPtrW(hWnd: isize, nIndex: i32) -> isize;
        fn GetDC(hWnd: isize) -> isize;
        fn ReleaseDC(hWnd: isize, hDC: isize) -> i32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateFontW(
            cHeight: i32,
            cWidth: i32,
            cEscapement: i32,
            cOrientation: i32,
            cWeight: i32,
            bItalic: u32,
            bUnderline: u32,
            bStrikeOut: u32,
            iCharSet: u32,
            iOutPrecision: u32,
            iClipPrecision: u32,
            iQuality: u32,
            iPitchAndFamily: u32,
            pszFaceName: *const u16,
        ) -> isize;
        fn DeleteObject(ho: isize) -> i32;
        fn SetBkMode(hdc: isize, mode: i32) -> i32;
        fn GetDeviceCaps(hdc: isize, index: i32) -> i32;
    }

    #[link(name = "comctl32")]
    unsafe extern "system" {
        fn InitCommonControlsEx(picce: *const INITCOMMONCONTROLSEX) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleW(lpModuleName: *const u16) -> isize;
    }

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHBrowseForFolderW(lpbi: *mut BROWSEINFOW) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, pszPath: *mut u16) -> i32;
    }

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(pv: *mut c_void);
    }

    pub fn run_wizard(
        cfg: InstallConfig,
        py_bytes: Vec<u8>,
        uninst_bytes: Vec<u8>,
    ) -> Result<ExitCode> {
        unsafe {
            win::CoInitializeEx(null(), 0x2);

            let icc = INITCOMMONCONTROLSEX {
                dw_size: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dw_icc: 0x0000_00FF,
            };
            InitCommonControlsEx(&icc);

            let h_instance = GetModuleHandleW(null());
            let class_name = to_wide("GoodniceDotSetupWizardWnd");

            let hdc_screen = GetDC(0);
            let dpi_y = if hdc_screen != 0 {
                let d = GetDeviceCaps(hdc_screen, 90); // LOGPIXELSY
                ReleaseDC(0, hdc_screen);
                if d > 0 { d } else { 96 }
            } else {
                96
            };
            let scale = (dpi_y as f32) / 96.0;

            #[allow(clippy::manual_dangling_ptr)]
            let mut app_icon = LoadIconW(h_instance, 1usize as *const u16);
            if app_icon == 0 {
                app_icon = LoadIconW(0, IDI_APPLICATION);
            }

            let wc = WNDCLASSEXW {
                cb_size: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfn_wnd_proc: wnd_proc,
                cb_cls_extra: 0,
                cb_wnd_extra: 0,
                h_instance,
                h_icon: app_icon,
                h_cursor: LoadCursorW(0, IDC_ARROW),
                hbr_background: GetSysColorBrush(COLOR_BTNFACE),
                lpsz_menu_name: null(),
                lpsz_class_name: class_name.as_ptr(),
                h_icon_sm: app_icon,
            };
            RegisterClassExW(&wc);

            let font_face = to_wide("Microsoft YaHei UI");
            let font_normal = CreateFontW(
                -((13.0 * scale).round() as i32),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                font_face.as_ptr(),
            );
            let font_title = CreateFontW(
                -((16.0 * scale).round() as i32),
                0,
                0,
                0,
                700,
                0,
                0,
                0,
                1,
                0,
                0,
                5,
                0,
                font_face.as_ptr(),
            );

            let mut state = Box::new(WizardState {
                cfg,
                py_bytes,
                uninst_bytes,
                page: WizardPage::Welcome,
                scale,
                font_normal,
                font_title,
                white_brush: GetSysColorBrush(COLOR_WINDOW),
                app_icon,
                lbl_header_title: 0,
                lbl_header_sub: 0,
                lbl_welcome: 0,
                lbl_dir_desc: 0,
                edit_dir: 0,
                btn_browse: 0,
                lbl_tasks: 0,
                chk_startmenu: 0,
                chk_path: 0,
                chk_context: 0,
                lbl_progress_status: 0,
                progress_bar: 0,
                edit_log: 0,
                log_text: String::new(),
                lbl_finish: 0,
                btn_back: 0,
                btn_next: 0,
                btn_cancel: 0,
                install_succeeded: false,
            });

            let win_w = sc(540, scale);
            let win_h = sc(405, scale);
            let screen_w = GetSystemMetrics(0);
            let screen_h = GetSystemMetrics(1);
            let win_x = (screen_w - win_w) / 2;
            let win_y = (screen_h - win_h) / 2;

            let title = to_wide(&format!(
                "Dot (自动解密工具) v{} 安装向导",
                env!("CARGO_PKG_VERSION")
            ));

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                win_x,
                win_y,
                win_w,
                win_h,
                0,
                0,
                h_instance,
                (&mut *state) as *mut WizardState as *const c_void,
            );

            if hwnd == 0 {
                bail!("创建安装向导窗口失败");
            }

            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            DeleteObject(font_normal);
            DeleteObject(font_title);
            win::CoUninitialize();

            if state.install_succeeded {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::from(1))
            }
        }
    }

    fn sc(px: i32, scale: f32) -> i32 {
        (px as f32 * scale).round() as i32
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn create_ctrl(
        ex_style: u32,
        class: &str,
        text: &str,
        style: u32,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        parent: isize,
        id: usize,
        font: isize,
        scale: f32,
    ) -> isize {
        let cls = to_wide(class);
        let txt = to_wide(text);
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                cls.as_ptr(),
                txt.as_ptr(),
                style,
                sc(x, scale),
                sc(y, scale),
                sc(w, scale),
                sc(h, scale),
                parent,
                id as isize,
                GetModuleHandleW(null()),
                null(),
            )
        };
        if hwnd != 0 && font != 0 {
            unsafe {
                SendMessageW(hwnd, WM_SETFONT, font as usize, 1);
            }
        }
        hwnd
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: isize,
        msg: u32,
        w_param: usize,
        l_param: isize,
    ) -> isize {
        unsafe {
            match msg {
                WM_CREATE => {
                    let create_struct = l_param as *const isize;
                    let state_ptr = *create_struct as *mut WizardState;
                    SetWindowLongPtrW(hwnd, -21, state_ptr as isize); // GWLP_USERDATA
                    let st = &mut *state_ptr;
                    init_controls(hwnd, st);
                    update_page_visibility(hwnd, st);
                    0
                }
                WM_PAINT => {
                    let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WizardState;
                    let mut ps: PAINTSTRUCT = std::mem::zeroed();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    if !ptr.is_null() {
                        let st = &*ptr;
                        let header_rc = RECT {
                            left: 0,
                            top: 0,
                            right: sc(540, st.scale),
                            bottom: sc(66, st.scale),
                        };
                        FillRect(hdc, &header_rc, st.white_brush);
                        if st.app_icon != 0 {
                            let icon_sz = sc(38, st.scale);
                            DrawIconEx(
                                hdc,
                                sc(480, st.scale),
                                sc(14, st.scale),
                                st.app_icon,
                                icon_sz,
                                icon_sz,
                                0,
                                0,
                                0x0003, // DI_NORMAL
                            );
                        }
                    }
                    EndPaint(hwnd, &ps);
                    0
                }
                WM_CTLCOLORSTATIC => {
                    let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WizardState;
                    if !ptr.is_null() {
                        let st = &*ptr;
                        let hdc = w_param as isize;
                        let ctrl = l_param;
                        if ctrl == st.lbl_header_title || ctrl == st.lbl_header_sub {
                            SetBkMode(hdc, TRANSPARENT);
                            return st.white_brush;
                        }
                    }
                    DefWindowProcW(hwnd, msg, w_param, l_param)
                }
                WM_COMMAND => {
                    let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WizardState;
                    if !ptr.is_null() {
                        let st = &mut *ptr;
                        let ctrl_id = w_param & 0xFFFF;
                        handle_command(hwnd, st, ctrl_id);
                    }
                    0
                }
                WM_APP_START_INSTALL => {
                    let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WizardState;
                    if !ptr.is_null() {
                        let st = &mut *ptr;
                        perform_installation(hwnd, st);
                    }
                    0
                }
                WM_CLOSE => {
                    let ptr = GetWindowLongPtrW(hwnd, -21) as *mut WizardState;
                    if !ptr.is_null() {
                        let st = &*ptr;
                        if st.page == WizardPage::Installing {
                            return 0;
                        }
                        if st.page != WizardPage::Finished {
                            let res = win::message_box(
                                hwnd,
                                "安装尚未完成。如果现在退出，程序将不会被安装。\n\n您确定要退出安装向导吗？",
                                "退出安装向导",
                                win::MB_YESNO | win::MB_ICONQUESTION,
                            );
                            if res != win::IDYES {
                                return 0;
                            }
                        }
                    }
                    DestroyWindow(hwnd);
                    0
                }
                WM_DESTROY => {
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, w_param, l_param),
            }
        }
    }

    unsafe fn init_controls(hwnd: isize, st: &mut WizardState) {
        let s = st.scale;
        let fn_norm = st.font_normal;
        let fn_title = st.font_title;

        unsafe {
            st.lbl_header_title = create_ctrl(
                0,
                "STATIC",
                "",
                WS_CHILD | WS_VISIBLE,
                22,
                12,
                490,
                24,
                hwnd,
                0,
                fn_title,
                s,
            );
            st.lbl_header_sub = create_ctrl(
                0,
                "STATIC",
                "",
                WS_CHILD | WS_VISIBLE,
                32,
                38,
                480,
                20,
                hwnd,
                0,
                fn_norm,
                s,
            );

            // Top & bottom separators
            create_ctrl(
                0,
                "STATIC",
                "",
                WS_CHILD | WS_VISIBLE | SS_ETCHEDHORZ,
                0,
                66,
                540,
                2,
                hwnd,
                0,
                0,
                s,
            );
            create_ctrl(
                0,
                "STATIC",
                "",
                WS_CHILD | WS_VISIBLE | SS_ETCHEDHORZ,
                0,
                316,
                540,
                2,
                hwnd,
                0,
                0,
                s,
            );

            // Page 0: Welcome
            let welcome_txt = concat!(
                "欢迎使用 Dot (自动解密工具) 安装向导。\r\n\r\n",
                "本程序将在您的计算机中配置以下功能：\r\n\r\n",
                "  • 安装自动解密核心引擎（隐藏内部可执行文件，不干扰系统 Python 环境）\r\n",
                "  • 在系统“开始”菜单中创建 dot 快捷方式\r\n",
                "  • 将快捷方式所在目录注入系统 PATH，在任意终端输入 dot 即可调用\r\n",
                "  • 在文件、文件夹及目录空白处注入“自动解密”右键菜单（后台静默运行，无弹窗）\r\n\r\n",
                "单击“下一步”继续，或单击“取消”退出安装向导。"
            );
            st.lbl_welcome = create_ctrl(
                0,
                "STATIC",
                welcome_txt,
                WS_CHILD,
                26,
                86,
                480,
                210,
                hwnd,
                0,
                fn_norm,
                s,
            );

            // Page 1: Options
            st.lbl_dir_desc = create_ctrl(
                0,
                "STATIC",
                "安装程序会将 Dot 安装到以下文件夹中。若要安装到其它位置，请单击“浏览”选择：",
                WS_CHILD,
                26,
                84,
                480,
                36,
                hwnd,
                0,
                fn_norm,
                s,
            );
            st.edit_dir = create_ctrl(
                WS_EX_CLIENTEDGE,
                "EDIT",
                &st.cfg.install_dir.to_string_lossy(),
                WS_CHILD | WS_TABSTOP | ES_AUTOHSCROLL,
                26,
                122,
                385,
                25,
                hwnd,
                IDC_EDIT_DIR,
                fn_norm,
                s,
            );
            st.btn_browse = create_ctrl(
                0,
                "BUTTON",
                "浏览(B)...",
                WS_CHILD | WS_TABSTOP,
                420,
                121,
                86,
                27,
                hwnd,
                IDC_BTN_BROWSE,
                fn_norm,
                s,
            );

            st.lbl_tasks = create_ctrl(
                0,
                "STATIC",
                "系统集成选项：",
                WS_CHILD,
                26,
                168,
                480,
                20,
                hwnd,
                0,
                fn_norm,
                s,
            );
            st.chk_startmenu = create_ctrl(
                0,
                "BUTTON",
                "在“开始”菜单创建 dot 快捷方式（必选）",
                WS_CHILD | BS_AUTOCHECKBOX | WS_DISABLED,
                34,
                194,
                460,
                22,
                hwnd,
                IDC_CHK_STARTMENU,
                fn_norm,
                s,
            );
            st.chk_path = create_ctrl(
                0,
                "BUTTON",
                "将“开始”菜单快捷方式目录加入系统 PATH 环境变量（支持命令行输入 dot）",
                WS_CHILD | WS_TABSTOP | BS_AUTOCHECKBOX,
                34,
                222,
                470,
                22,
                hwnd,
                IDC_CHK_PATH,
                fn_norm,
                s,
            );
            st.chk_context = create_ctrl(
                0,
                "BUTTON",
                "添加资源管理器右键菜单“自动解密”（支持文件与文件夹，无弹窗静默执行）",
                WS_CHILD | WS_TABSTOP | BS_AUTOCHECKBOX,
                34,
                250,
                470,
                22,
                hwnd,
                IDC_CHK_CONTEXT,
                fn_norm,
                s,
            );
            SendMessageW(st.chk_startmenu, BM_SETCHECK, BST_CHECKED, 0);
            SendMessageW(st.chk_path, BM_SETCHECK, BST_CHECKED, 0);
            SendMessageW(st.chk_context, BM_SETCHECK, BST_CHECKED, 0);

            // Page 2: Installing
            st.lbl_progress_status = create_ctrl(
                0,
                "STATIC",
                "正在准备安装...",
                WS_CHILD,
                26,
                84,
                480,
                20,
                hwnd,
                0,
                fn_norm,
                s,
            );
            st.progress_bar = create_ctrl(
                0,
                "msctls_progress32",
                "",
                WS_CHILD | WS_BORDER,
                26,
                110,
                480,
                22,
                hwnd,
                0,
                fn_norm,
                s,
            );
            SendMessageW(st.progress_bar, PBM_SETRANGE32, 0, 100);
            st.edit_log = create_ctrl(
                WS_EX_CLIENTEDGE,
                "EDIT",
                "",
                WS_CHILD | WS_VSCROLL | ES_MULTILINE | ES_READONLY,
                26,
                144,
                480,
                154,
                hwnd,
                0,
                fn_norm,
                s,
            );

            // Page 3: Finished
            let finish_txt = concat!(
                "Dot (自动解密工具) 已成功安装在您的计算机上！\r\n\r\n",
                "您现在可以通过以下方式使用：\r\n\r\n",
                "  • 右键菜单：在任意文件、文件夹或目录空白处右键点击“自动解密”，将在后台静默完成解密，不会弹出黑框窗口。\r\n\r\n",
                "  • 命令行终端：打开新的 CMD 或 PowerShell 终端，输入 `dot <文件或目录>`（支持 `-r` 递归）。\r\n\r\n",
                "单击“完成”关闭安装向导。"
            );
            st.lbl_finish = create_ctrl(
                0,
                "STATIC",
                finish_txt,
                WS_CHILD,
                26,
                86,
                480,
                210,
                hwnd,
                0,
                fn_norm,
                s,
            );

            // Footer buttons
            st.btn_back = create_ctrl(
                0,
                "BUTTON",
                "< 上一步(B)",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                238,
                330,
                90,
                28,
                hwnd,
                IDC_BTN_BACK,
                fn_norm,
                s,
            );
            st.btn_next = create_ctrl(
                0,
                "BUTTON",
                "下一步(N) >",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_DEFPUSHBUTTON,
                334,
                330,
                90,
                28,
                hwnd,
                IDC_BTN_NEXT,
                fn_norm,
                s,
            );
            st.btn_cancel = create_ctrl(
                0,
                "BUTTON",
                "取消",
                WS_CHILD | WS_VISIBLE | WS_TABSTOP,
                434,
                330,
                82,
                28,
                hwnd,
                IDC_BTN_CANCEL,
                fn_norm,
                s,
            );
        }
    }

    unsafe fn set_text(hwnd: isize, text: &str) {
        let w = to_wide(text);
        unsafe {
            SetWindowTextW(hwnd, w.as_ptr());
        }
    }

    unsafe fn get_text(hwnd: isize) -> String {
        unsafe {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                return String::new();
            }
            let mut buf = vec![0u16; (len + 1) as usize];
            let read = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
            buf.truncate(read as usize);
            String::from_utf16_lossy(&buf)
        }
    }

    unsafe fn show(hwnd: isize, visible: bool) {
        unsafe {
            ShowWindow(hwnd, if visible { SW_SHOW } else { SW_HIDE });
        }
    }

    unsafe fn update_page_visibility(_hwnd: isize, st: &mut WizardState) {
        unsafe {
            let is_welcome = st.page == WizardPage::Welcome;
            let is_options = st.page == WizardPage::Options;
            let is_installing = st.page == WizardPage::Installing;
            let is_finished = st.page == WizardPage::Finished;

            show(st.lbl_welcome, is_welcome);

            show(st.lbl_dir_desc, is_options);
            show(st.edit_dir, is_options);
            show(st.btn_browse, is_options);
            show(st.lbl_tasks, is_options);
            show(st.chk_startmenu, is_options);
            show(st.chk_path, is_options);
            show(st.chk_context, is_options);

            show(st.lbl_progress_status, is_installing);
            show(st.progress_bar, is_installing);
            show(st.edit_log, is_installing);

            show(st.lbl_finish, is_finished);

            match st.page {
                WizardPage::Welcome => {
                    set_text(st.lbl_header_title, "欢迎使用 Dot (自动解密工具) 安装向导");
                    set_text(
                        st.lbl_header_sub,
                        "本向导将引导您完成 Dot (自动解密工具) 的安装与系统集成",
                    );
                    EnableWindow(st.btn_back, 0);
                    EnableWindow(st.btn_next, 1);
                    EnableWindow(st.btn_cancel, 1);
                    set_text(st.btn_next, "下一步(N) >");
                }
                WizardPage::Options => {
                    set_text(st.lbl_header_title, "选择安装位置与附加任务");
                    set_text(
                        st.lbl_header_sub,
                        "设置程序安装目录并勾选需要注入的系统功能",
                    );
                    EnableWindow(st.btn_back, 1);
                    EnableWindow(st.btn_next, 1);
                    EnableWindow(st.btn_cancel, 1);
                    set_text(st.btn_next, "安装(I)");
                }
                WizardPage::Installing => {
                    set_text(st.lbl_header_title, "正在安装");
                    set_text(
                        st.lbl_header_sub,
                        "正在安装 Dot (自动解密工具) 并配置系统环境，请稍候...",
                    );
                    EnableWindow(st.btn_back, 0);
                    EnableWindow(st.btn_next, 0);
                    EnableWindow(st.btn_cancel, 0);
                }
                WizardPage::Finished => {
                    set_text(st.lbl_header_title, "Dot (自动解密工具) 安装完成");
                    set_text(
                        st.lbl_header_sub,
                        "安装向导已在您的计算机上成功安装并配置 Dot",
                    );
                    show(st.btn_back, false);
                    EnableWindow(st.btn_next, 1);
                    EnableWindow(st.btn_cancel, 0);
                    set_text(st.btn_next, "完成(F)");
                }
            }
        }
    }

    unsafe fn handle_command(hwnd: isize, st: &mut WizardState, ctrl_id: usize) {
        unsafe {
            match ctrl_id {
                IDC_BTN_BROWSE => {
                    if let Some(chosen) = browse_for_folder(hwnd, "请选择 Dot 的安装目录：") {
                        let target = if chosen
                            .file_name()
                            .map(|n| n.eq_ignore_ascii_case("Dot"))
                            .unwrap_or(false)
                        {
                            chosen
                        } else {
                            chosen.join("Dot")
                        };
                        set_text(st.edit_dir, &target.to_string_lossy());
                    }
                }
                IDC_BTN_BACK => {
                    if st.page == WizardPage::Options {
                        st.page = WizardPage::Welcome;
                        update_page_visibility(hwnd, st);
                    }
                }
                IDC_BTN_NEXT => match st.page {
                    WizardPage::Welcome => {
                        st.page = WizardPage::Options;
                        update_page_visibility(hwnd, st);
                    }
                    WizardPage::Options => {
                        let dir_str = get_text(st.edit_dir).trim().to_string();
                        if dir_str.is_empty() {
                            win::message_box(
                                hwnd,
                                "安装目录不能为空，请选择有效的目标文件夹。",
                                "提示",
                                win::MB_OK | win::MB_ICONHAND,
                            );
                            return;
                        }
                        st.cfg.install_dir = PathBuf::from(dir_str);
                        st.cfg.enable_path_env =
                            SendMessageW(st.chk_path, BM_GETCHECK, 0, 0) as usize == BST_CHECKED;
                        st.cfg.enable_context_menu =
                            SendMessageW(st.chk_context, BM_GETCHECK, 0, 0) as usize == BST_CHECKED;

                        st.page = WizardPage::Installing;
                        update_page_visibility(hwnd, st);
                        PostMessageW(hwnd, WM_APP_START_INSTALL, 0, 0);
                    }
                    WizardPage::Finished => {
                        DestroyWindow(hwnd);
                    }
                    WizardPage::Installing => {}
                },
                IDC_BTN_CANCEL => {
                    SendMessageW(hwnd, WM_CLOSE, 0, 0);
                }
                _ => {}
            }
        }
    }

    unsafe fn perform_installation(hwnd: isize, st: &mut WizardState) {
        unsafe {
            let cfg_clone = st.cfg.clone();
            let py_clone = st.py_bytes.clone();
            let uninst_clone = st.uninst_bytes.clone();

            let result = install_with_progress(&cfg_clone, &py_clone, &uninst_clone, |pct, msg| {
                set_text(st.lbl_progress_status, msg);
                SendMessageW(st.progress_bar, PBM_SETPOS, pct as usize, 0);
                if !st.log_text.is_empty() {
                    st.log_text.push_str("\r\n");
                }
                st.log_text.push_str("• ");
                st.log_text.push_str(msg);
                set_text(st.edit_log, &st.log_text);
                pump_messages();
            });

            match result {
                Ok(()) => {
                    st.install_succeeded = true;
                    st.page = WizardPage::Finished;
                    update_page_visibility(hwnd, st);
                }
                Err(err) => {
                    win::message_box(
                        hwnd,
                        &format!("安装失败：\n\n{err:#}"),
                        "Dot 安装错误",
                        win::MB_OK | win::MB_ICONHAND,
                    );
                    st.page = WizardPage::Options;
                    update_page_visibility(hwnd, st);
                }
            }
        }
    }

    unsafe fn pump_messages() {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while PeekMessageW(&mut msg, 0, 0, 0, 1) != 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe fn browse_for_folder(hwnd: isize, title: &str) -> Option<PathBuf> {
        unsafe {
            let mut display_buf = [0u16; 260];
            let title_w = to_wide(title);
            let mut bi = BROWSEINFOW {
                hwnd_owner: hwnd,
                pidl_root: null(),
                psz_display_name: display_buf.as_mut_ptr(),
                lpsz_title: title_w.as_ptr(),
                ul_flags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
                lpfn: null(),
                l_param: 0,
                i_image: 0,
            };
            let pidl = SHBrowseForFolderW(&mut bi);
            if pidl.is_null() {
                return None;
            }
            let mut path_buf = [0u16; 260];
            let ok = SHGetPathFromIDListW(pidl, path_buf.as_mut_ptr());
            CoTaskMemFree(pidl);
            if ok == 0 {
                return None;
            }
            let len = path_buf.iter().position(|&c| c == 0).unwrap_or(0);
            if len == 0 {
                return None;
            }
            Some(PathBuf::from(String::from_utf16_lossy(&path_buf[..len])))
        }
    }
}
