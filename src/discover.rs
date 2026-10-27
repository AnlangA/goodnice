use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use walkdir::WalkDir;

use crate::pathutil::{
    absolute, extension_allowed, is_office_lock_file, is_tool_temp, same_path,
};

const DEFAULT_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "target",
    "node_modules",
    "$recycle.bin",
    "system volume information",
    ".dot-cache",
    "goodnice-cache",
];

pub struct DiscoverOptions<'a> {
    pub recursive: bool,
    /// 已规范化（无点、小写）的扩展名列表；空表示不过滤。
    pub extensions: &'a [String],
    pub extra_exclude_dirs: &'a [String],
    pub output_dir: &'a Path,
}

pub fn collect_files(root: &Path, opts: &DiscoverOptions<'_>) -> Result<Vec<PathBuf>> {
    let mut files = if opts.recursive {
        collect_recursive(root, opts)?
    } else {
        collect_top_level(root, opts)?
    };
    files.sort();
    files.dedup();
    Ok(files)
}

fn should_skip_dir(path: &Path, opts: &DiscoverOptions<'_>) -> bool {
    if same_path(path, opts.output_dir) {
        return true;
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if DEFAULT_EXCLUDE_DIRS.contains(&name.as_str()) {
        return true;
    }
    opts.extra_exclude_dirs
        .iter()
        .any(|n| n.eq_ignore_ascii_case(&name))
}

fn accept_file(path: &Path, opts: &DiscoverOptions<'_>) -> bool {
    if !path.is_file() {
        return false;
    }
    if is_tool_temp(path) || is_office_lock_file(path) {
        return false;
    }
    if is_under_output(path, opts.output_dir) {
        return false;
    }
    extension_allowed(path, opts.extensions)
}

fn is_under_output(path: &Path, output_dir: &Path) -> bool {
    crate::pathutil::is_within(path, output_dir)
}

fn collect_top_level(root: &Path, opts: &DiscoverOptions<'_>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let entries =
        fs::read_dir(root).with_context(|| format!("无法读取目录: {}", root.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("读取目录项失败: {}", root.display()))?;
        let path = entry.path();
        if path.is_dir() {
            continue;
        }
        if accept_file(&path, opts) {
            files.push(absolute(&path).unwrap_or(path));
        }
    }
    Ok(files)
}

fn collect_recursive(root: &Path, opts: &DiscoverOptions<'_>) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let walker = WalkDir::new(root).follow_links(false).into_iter();
    for entry in walker.filter_entry(|e| {
        if e.file_type().is_dir() {
            // 根目录本身不要按名字过滤
            if e.depth() == 0 {
                return true;
            }
            !should_skip_dir(e.path(), opts)
        } else {
            true
        }
    }) {
        let entry = entry.with_context(|| format!("遍历目录失败: {}", root.display()))?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if accept_file(path, opts) {
            files.push(absolute(path).unwrap_or_else(|_| path.to_path_buf()));
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use tempfile::tempdir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        File::create(path).unwrap();
    }

    #[test]
    fn top_level_skips_subdirs_and_output() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.pdf"));
        touch(&root.join("b.txt"));
        touch(&root.join("sub").join("c.pdf"));
        let output = root.join("output");
        fs::create_dir(&output).unwrap();
        touch(&output.join("old.pdf"));

        let exts: Vec<String> = Vec::new();
        let extra: Vec<String> = Vec::new();
        let files = collect_files(
            root,
            &DiscoverOptions {
                recursive: false,
                extensions: &exts,
                extra_exclude_dirs: &extra,
                output_dir: &output,
            },
        )
        .unwrap();

        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.pdf".into()));
        assert!(names.contains(&"b.txt".into()));
        assert!(!names.contains(&"c.pdf".into()));
        assert!(!names.contains(&"old.pdf".into()));
    }

    #[test]
    fn recursive_walks_subdirs_skips_git_and_filters_ext() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        touch(&root.join("a.pdf"));
        touch(&root.join("b.txt"));
        touch(&root.join("sub").join("c.pdf"));
        touch(&root.join("sub").join("d.xlsx"));
        touch(&root.join(".git").join("config"));
        touch(&root.join("target").join("out"));
        let output = root.join("output");

        let exts = vec!["pdf".into()];
        let extra: Vec<String> = Vec::new();
        let files = collect_files(
            root,
            &DiscoverOptions {
                recursive: true,
                extensions: &exts,
                extra_exclude_dirs: &extra,
                output_dir: &output,
            },
        )
        .unwrap();

        let names: Vec<_> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.pdf".into()));
        assert!(names.contains(&"c.pdf".into()));
        assert!(!names.contains(&"b.txt".into()));
        assert!(!names.contains(&"d.xlsx".into()));
        assert!(!names.contains(&"config".into()));
        assert!(!names.contains(&"out".into()));
    }
}
