use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=GOODNICE_EMBED_PYTHON_EXE");
    println!("cargo:rerun-if-env-changed=GOODNICE_EMBED_UNINSTALL_EXE");

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
