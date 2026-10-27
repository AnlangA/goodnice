use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-env-changed=GOODNICE_EMBED_PYTHON_EXE");
    println!("cargo:rerun-if-env-changed=GOODNICE_EMBED_UNINSTALL_EXE");
    println!("cargo:rerun-if-changed=src/icon.rs");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let dest_path = out_dir.join("embedded_payload.rs");

    let py_exe = resolve_staged_binary("GOODNICE_EMBED_PYTHON_EXE");
    let uninst_exe = resolve_staged_binary("GOODNICE_EMBED_UNINSTALL_EXE");

    let is_packaged = py_exe.is_some() && uninst_exe.is_some();

    let mut code = String::new();
    match (py_exe, uninst_exe) {
        (Some(py), Some(uninst)) => {
            println!("cargo:rerun-if-changed={}", py.display());
            println!("cargo:rerun-if-changed={}", uninst.display());
            let py_lit = format!("{:?}", py.to_string_lossy());
            let uninst_lit = format!("{:?}", uninst.to_string_lossy());
            code.push_str(&format!(
                "pub const EMBEDDED_PYTHON_EXE: Option<&[u8]> = Some(include_bytes!({py_lit}));\n\
                 pub const EMBEDDED_UNINSTALL_EXE: Option<&[u8]> = Some(include_bytes!({uninst_lit}));\n"
            ));
        }
        _ => {
            code.push_str(
                "pub const EMBEDDED_PYTHON_EXE: Option<&[u8]> = None;\n\
                 pub const EMBEDDED_UNINSTALL_EXE: Option<&[u8]> = None;\n",
            );
        }
    }

    fs::write(&dest_path, code).expect("failed to write embedded_payload.rs");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        // 生成解开的锁 (🔓) .ico 图标并通过 rc.exe 编译为 PE 图标资源
        let ico_path = out_dir.join("dot.ico");
        let ico_bytes = icon::generate_unlocked_padlock_ico();
        fs::write(&ico_path, ico_bytes).expect("failed to write dot.ico");

        if let Some(res_path) = compile_icon_resource(&out_dir, &ico_path) {
            for bin in ["python", "setup", "uninstall"] {
                println!("cargo:rustc-link-arg-bin={bin}={}", res_path.display());
            }
        }

        let manifest_xml = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0" xmlns:asmv3="urn:schemas-microsoft-com:asm.v3">
  <assemblyIdentity type="win32" name="Goodnice.Dot.Installer" version="0.1.0.0" processorArchitecture="*"/>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
  <asmv3:application>
    <asmv3:windowsSettings>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2,System</dpiAwareness>
    </asmv3:windowsSettings>
  </asmv3:application>
</assembly>
"#;

        let manifest_path = out_dir.join("installer.manifest");
        fs::write(&manifest_path, manifest_xml).expect("failed to write installer.manifest");

        for bin in ["setup", "uninstall"] {
            println!("cargo:rustc-link-arg-bin={bin}=/MANIFEST:EMBED");
            if is_packaged {
                println!(
                    "cargo:rustc-link-arg-bin={bin}=/MANIFESTUAC:level='requireAdministrator'"
                );
            }
            println!(
                "cargo:rustc-link-arg-bin={bin}=/MANIFESTINPUT:{}",
                manifest_path.display()
            );
        }
    }
}

fn compile_icon_resource(out_dir: &Path, ico_path: &Path) -> Option<PathBuf> {
    let rc_exe = find_rc_exe()?;
    let rc_path = out_dir.join("app_icon.rc");
    let res_path = out_dir.join("app_icon.res");
    let ico_escaped = ico_path.to_string_lossy().replace('\\', "\\\\");
    let rc_content = format!("1 ICON \"{ico_escaped}\"\r\n");
    fs::write(&rc_path, rc_content).ok()?;

    let status = Command::new(rc_exe)
        .args(["/nologo", "/fo"])
        .arg(&res_path)
        .arg(&rc_path)
        .status()
        .ok()?;

    if status.success() && res_path.is_file() {
        Some(res_path)
    } else {
        None
    }
}

fn find_rc_exe() -> Option<PathBuf> {
    if let Ok(status) = Command::new("rc.exe").arg("/?").output()
        && status.status.success()
    {
        return Some(PathBuf::from("rc.exe"));
    }

    let kits_root = Path::new(r"C:\Program Files (x86)\Windows Kits\10\bin");
    if !kits_root.is_dir() {
        return None;
    }

    let mut versions: Vec<PathBuf> = fs::read_dir(kits_root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    versions.sort();
    versions.reverse();

    for ver_dir in versions {
        let candidate = ver_dir.join("x64").join("rc.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn resolve_staged_binary(env_key: &str) -> Option<PathBuf> {
    let val = env::var(env_key).ok()?;
    let trimmed = val.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    if path.is_file() {
        fs::canonicalize(path).ok().or_else(|| Some(path.to_path_buf()))
    } else {
        None
    }
}
