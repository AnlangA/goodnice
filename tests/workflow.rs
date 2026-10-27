#![cfg(windows)]

use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::process::{Command, Output};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("goodnice-workflow-")
        .tempdir_in(Path::new(env!("CARGO_MANIFEST_DIR")).join("target"))
        .unwrap()
}

fn run(source: &Path, output: &Path, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_python"))
        .arg(source)
        .arg("-o")
        .arg(output)
        .args(flags)
        .output()
        .unwrap()
}

fn assert_success(result: &Output) {
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn recursive_processing_restores_original_paths_and_deletes_output_tree() {
    let dir = tempdir();
    let source = dir.path().join("docs");
    let output = source.join("output");
    fs::create_dir_all(source.join("sub")).unwrap();
    let fixtures: &[(&str, &[u8])] = &[
        ("report.pdf", b"pdf\0\xff"),
        ("report.xlsx", b"xlsx\0\xfe"),
        ("sub/report.pdf", b"nested pdf"),
        ("sub/大写.XLSX", b"uppercase"),
        ("sub/no-extension", b""),
    ];
    for (name, data) in fixtures {
        fs::write(source.join(name), data).unwrap();
    }

    let result = run(&source, &output, &["-r"]);

    assert_success(&result);
    assert!(!output.exists());
    for (name, data) in fixtures {
        assert_eq!(fs::read(source.join(name)).unwrap(), *data);
    }
    assert_eq!(fs::read_dir(&source).unwrap().count(), 3);
    assert_eq!(fs::read_dir(source.join("sub")).unwrap().count(), 3);
}

#[test]
fn single_file_restores_source_and_removes_separate_output_directory() {
    let source_dir = tempdir();
    let output_parent = tempdir();
    let source = source_dir.path().join("a.pdf");
    let output = output_parent.path().join("output");
    fs::write(&source, b"single-file payload").unwrap();

    let result = run(&source, &output, &[]);

    assert_success(&result);
    assert_eq!(fs::read(&source).unwrap(), b"single-file payload");
    assert!(!output.exists());
    assert!(String::from_utf8_lossy(&result.stdout).contains(&source.display().to_string()));
}

#[test]
fn existing_unrelated_output_files_are_preserved() {
    let dir = tempdir();
    let source = dir.path().join("a.xlsx");
    let output = dir.path().join("output");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&output).unwrap();
    fs::write(output.join("keep.txt"), b"keep").unwrap();

    let result = run(&source, &output, &[]);

    assert_success(&result);
    assert_eq!(fs::read(source).unwrap(), b"original");
    assert_eq!(fs::read(output.join("keep.txt")).unwrap(), b"keep");
    assert!(!output.join("a.xlsx").exists());
    assert!(String::from_utf8_lossy(&result.stderr).contains("已保留"));
}

#[test]
fn dry_run_does_not_replace_files_or_remove_output_directory() {
    let dir = tempdir();
    let source = dir.path().join("a.pdf");
    let output = dir.path().join("output");
    fs::write(&source, b"original").unwrap();
    fs::create_dir(&output).unwrap();

    let result = run(&source, &output, &["--dry-run"]);

    assert_success(&result);
    assert_eq!(fs::read(source).unwrap(), b"original");
    assert!(output.is_dir());
    assert_eq!(fs::read_dir(output).unwrap().count(), 0);
}

#[test]
fn failed_source_deletion_keeps_verified_output() {
    let dir = tempdir();
    let source = dir.path().join("a.xlsx");
    let output = dir.path().join("output");
    fs::write(&source, b"original").unwrap();
    // 允许读写但禁止删除，可靠模拟源文件被其他程序占用的情况。
    let locked_source = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&source)
        .unwrap();

    for flags in [&[][..], &["--fail-fast"][..]] {
        let result = run(&source, &output, flags);
        assert!(!result.status.success());
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(fs::read(output.join("a.xlsx")).unwrap(), b"original");
    }
    drop(locked_source);
}

#[test]
fn later_file_with_same_name_does_not_overwrite_failed_output() {
    let dir = tempdir();
    let first = dir.path().join("first").join("a.xlsx");
    let second = dir.path().join("second").join("a.xlsx");
    let output = dir.path().join("output");
    fs::create_dir(first.parent().unwrap()).unwrap();
    fs::create_dir(second.parent().unwrap()).unwrap();
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();
    let locked_source = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&first)
        .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_python"))
        .args([&first, &second])
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();

    assert!(!result.status.success());
    assert_eq!(fs::read(&first).unwrap(), b"first");
    assert_eq!(fs::read(&second).unwrap(), b"second");
    assert_eq!(fs::read(output.join("a.xlsx")).unwrap(), b"first");
    drop(locked_source);
}
