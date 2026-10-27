use std::env;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// 与 Python `pathlib.Path.suffix` 一致：只取最后一段扩展名；
/// `.gitignore` 这类以点开头且无其它点的名字视为无扩展名。
pub fn suffix(path: &Path) -> String {
    let name = match path.file_name() {
        Some(n) => n.to_string_lossy(),
        None => return String::new(),
    };
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!(".{ext}"),
        _ => String::new(),
    }
}

/// 与 Python `Path.with_suffix` 一致。`new_suffix` 应带点，如 `.xlsx`；
/// 空字符串表示去掉扩展名。
#[cfg(test)]
pub fn with_suffix(path: &Path, new_suffix: &str) -> PathBuf {
    let name = match path.file_name() {
        Some(n) => n.to_string_lossy().into_owned(),
        None => return path.to_path_buf(),
    };
    let new_name = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => format!("{stem}{new_suffix}"),
        _ => format!("{name}{new_suffix}"),
    };
    path.with_file_name(new_name)
}

pub fn stem(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_string(),
        _ => name,
    }
}

pub fn is_xlsx(path: &Path) -> bool {
    suffix(path).eq_ignore_ascii_case(".xlsx")
}

/// 本工具生成的中间 xlsx 副本：`{stem}.goodnice-{n}.xlsx`
pub const TEMP_MARK: &str = ".goodnice-";

pub fn is_tool_temp(path: &Path) -> bool {
    path.file_name()
        .map(|n| {
            let n = n.to_string_lossy();
            n.contains(TEMP_MARK) && n.to_ascii_lowercase().ends_with(".xlsx")
        })
        .unwrap_or(false)
}

#[cfg(test)]
fn unique_marked_xlsx(dir: &Path, file_stem: &str) -> PathBuf {
    for n in 1u32.. {
        let name = format!("{file_stem}{TEMP_MARK}{n}.xlsx");
        let p = dir.join(name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(format!("{file_stem}{TEMP_MARK}fallback.xlsx"))
}

/// 非 xlsx 源文件的中间副本。始终带 `.goodnice-` 标记，绝不占用用户已有的同名 `.xlsx`。
#[cfg(test)]
pub fn xlsx_intermediate_path(source: &Path) -> PathBuf {
    let parent = source.parent().unwrap_or_else(|| Path::new("."));
    unique_marked_xlsx(parent, &stem(source))
}

/// 在系统临时目录（`%TEMP%\goodnice-cache`）中生成唯一的临时 `.xlsx` 内存缓存句柄路径，
/// 确保用户源目录中绝不产生任何 `.xlsx` 中间文件。
pub fn ram_temp_xlsx_path(source: &Path) -> PathBuf {
    let base = env::temp_dir().join("goodnice-cache");
    let pid = process::id();
    let file_stem = stem(source);
    loop {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!("{file_stem}{TEMP_MARK}{pid}-{seq}.xlsx"));
        if !candidate.exists() {
            return candidate;
        }
    }
}

/// 转成绝对路径，但不调用 canonicalize，避免 Windows 上出现 `\\?\` 前缀。
pub fn absolute(path: &Path) -> io::Result<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()?.join(path)
    };
    Ok(normalize(&joined))
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() && !path.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn eq_component(a: &Component, b: &Component) -> bool {
    #[cfg(windows)]
    {
        a.as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&b.as_os_str().to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

/// 按组件剥离前缀。Windows 上目录名大小写不敏感。
pub fn strip_prefix_ci(source: &Path, root: &Path) -> Option<PathBuf> {
    let mut source_comps = source.components();
    for pc in root.components() {
        match source_comps.next() {
            Some(cc) if eq_component(&cc, &pc) => {}
            _ => return None,
        }
    }
    Some(source_comps.as_path().to_path_buf())
}

/// `child` 是否就是 `parent`，或位于其目录树下。
pub fn is_within(child: &Path, parent: &Path) -> bool {
    let child = absolute(child).unwrap_or_else(|_| normalize(child));
    let parent = absolute(parent).unwrap_or_else(|_| normalize(parent));
    strip_prefix_ci(&child, &parent).is_some()
}

pub fn same_path(a: &Path, b: &Path) -> bool {
    is_within(a, b) && is_within(b, a)
}

/// 规范化用户给出的扩展名：去掉前导点并转小写。
pub fn normalize_ext(ext: &str) -> String {
    ext.trim().trim_start_matches('.').to_ascii_lowercase()
}

pub fn extension_allowed(path: &Path, exts: &[String]) -> bool {
    if exts.is_empty() {
        return true;
    }
    let suf = suffix(path);
    let got = suf.trim_start_matches('.').to_ascii_lowercase();
    exts.iter().any(|e| e == &got)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_matches_pathlib() {
        assert_eq!(suffix(Path::new("foo.pdf")), ".pdf");
        assert_eq!(suffix(Path::new("foo.bar.pdf")), ".pdf");
        assert_eq!(suffix(Path::new("foo")), "");
        assert_eq!(suffix(Path::new(".gitignore")), "");
        assert_eq!(suffix(Path::new("foo.XLSX")), ".XLSX");
    }

    #[test]
    fn with_suffix_matches_pathlib() {
        assert_eq!(
            with_suffix(Path::new("foo.pdf"), ".xlsx"),
            PathBuf::from("foo.xlsx")
        );
        assert_eq!(
            with_suffix(Path::new("foo.bar.pdf"), ".xlsx"),
            PathBuf::from("foo.bar.xlsx")
        );
        assert_eq!(
            with_suffix(Path::new("foo"), ".xlsx"),
            PathBuf::from("foo.xlsx")
        );
        assert_eq!(
            with_suffix(Path::new(".gitignore"), ".xlsx"),
            PathBuf::from(".gitignore.xlsx")
        );
        assert_eq!(
            with_suffix(Path::new("dir/foo.pdf"), ""),
            PathBuf::from("dir/foo")
        );
    }

    #[test]
    fn xlsx_detection_is_case_insensitive() {
        assert!(is_xlsx(Path::new("a.xlsx")));
        assert!(is_xlsx(Path::new("a.XLSX")));
        assert!(!is_xlsx(Path::new("a.pdf")));
        assert!(!is_xlsx(Path::new("xlsx")));
    }

    #[test]
    fn tool_temp_detection() {
        assert!(is_tool_temp(Path::new("foo.goodnice-1.xlsx")));
        assert!(is_tool_temp(Path::new("foo.goodnice-write-1.xlsx")));
        assert!(!is_tool_temp(Path::new("foo.xlsx")));
        assert!(!is_tool_temp(Path::new("foo.goodnice-1.pdf")));
    }

    #[test]
    fn intermediate_never_clobbers_existing_xlsx() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("report.pdf");
        let xlsx = dir.path().join("report.xlsx");
        std::fs::write(&pdf, b"pdf").unwrap();
        std::fs::write(&xlsx, b"keep-me").unwrap();

        let tmp = xlsx_intermediate_path(&pdf);
        assert_ne!(tmp, xlsx);
        assert!(is_tool_temp(&tmp));
        assert_eq!(tmp.parent(), Some(dir.path()));
        assert!(!tmp.exists());
        assert_eq!(std::fs::read(&xlsx).unwrap(), b"keep-me");
    }

    #[test]
    fn is_within_nested_paths() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("docs");
        let child = parent.join("sub").join("a.pdf");
        let other = dir.path().join("other").join("a.pdf");
        let extra = dir.path().join("docs-extra").join("a");
        assert!(is_within(&child, &parent));
        assert!(is_within(&parent, &parent));
        assert!(!is_within(&other, &parent));
        assert!(!is_within(&extra, &parent));
    }

    #[test]
    fn strip_prefix_keeps_relative_tail() {
        let root = Path::new("docs");
        let source = Path::new("docs").join("sub").join("a.pdf");
        assert_eq!(
            strip_prefix_ci(&source, root),
            Some(PathBuf::from("sub").join("a.pdf"))
        );
    }
}
