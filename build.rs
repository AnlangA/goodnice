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
