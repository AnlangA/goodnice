mod cli;
mod discover;
mod pathutil;
mod powershell;
mod process;

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;

use cli::Cli;
use discover::{DiscoverOptions, collect_files};
use pathutil::{absolute, normalize_ext, same_path, strip_prefix_ci};
use process::{
    FileOutcome, ProcessConfig, cleanup_output_dir, planned_dest, process_file, validate_output_dir,
};

fn main() -> ExitCode {
    enable_windows_utf8();
    match run() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("错误: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let paths = resolve_paths(&cli)?;
    if paths.is_empty() {
        bail!("请指定至少一个文件或目录");
    }

    let output_dir = resolve_output_dir(&cli, &paths);
    let extensions: Vec<String> = cli.extension.iter().map(|e| normalize_ext(e)).collect();

    let powershell = if cli.dry_run {
        OsString::from("powershell.exe")
    } else {
        powershell::detect_powershell()?
    };

    let mut jobs: Vec<Job> = Vec::new();
    for path in &paths {
        if path.is_file() {
            if !pathutil::extension_allowed(path, &extensions) {
                eprintln!("跳过（扩展名不匹配）: {}", path.display());
                continue;
            }
            if pathutil::is_tool_temp(path) {
                eprintln!("跳过（中间临时文件）: {}", path.display());
                continue;
            }
            jobs.push(Job {
                source: absolute(path).unwrap_or_else(|_| path.clone()),
                dest: planned_dest(path, None, &output_dir),
                input_root: None,
            });
        } else if path.is_dir() {
            if same_path(path, &output_dir) {
                bail!(
                    "输入目录与输出目录相同: {}。请使用 -o 指定其他输出目录",
                    path.display()
                );
            }
            if pathutil::is_within(path, &output_dir) {
                bail!(
                    "输入目录位于输出目录内: {} ⊂ {}。请使用 -o 指定其他输出目录",
                    path.display(),
                    output_dir.display()
                );
            }
            let opts = DiscoverOptions {
                recursive: cli.recursive,
                extensions: &extensions,
                extra_exclude_dirs: &cli.exclude_dir,
                output_dir: &output_dir,
            };
            let files = collect_files(path, &opts)?;
            let root = absolute(path).unwrap_or_else(|_| path.clone());
            if files.is_empty() {
                let scope = if cli.recursive {
                    "（含所有子目录）"
                } else {
                    "（仅当前目录）"
                };
                eprintln!("未找到待处理文件{}: {}", scope, path.display());
                continue;
            }
            for file in files {
                let dest = planned_dest(&file, Some(&root), &output_dir);
                jobs.push(Job {
                    source: file,
                    dest,
                    input_root: Some(root.clone()),
                });
            }
        } else {
            bail!("路径不存在: {}", path.display());
        }
    }

    if jobs.is_empty() {
        println!("没有需要处理的文件");
        return Ok(ExitCode::SUCCESS);
    }

    // 先检查全部任务，避免处理到一半才发现源文件位于输出目录内。
    for job in &jobs {
        validate_output_dir(&job.source, &output_dir)?;
    }
    warn_destination_collisions(&jobs);

    if cli.verbose || cli.dry_run {
        let mode = if cli.dry_run {
            "预览"
        } else {
            "开始处理"
        };
        println!("{mode} {} 个文件 -> {}", jobs.len(), output_dir.display());
    }

    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut failed_outputs = Vec::new();
    let total = jobs.len();

    for (i, job) in jobs.iter().enumerate() {
        let idx = i + 1;
        let label = display_source(&job.source, job.input_root.as_deref());
        if !cli.verbose {
            print!("[{idx}/{total}] {label} ... ");
            let _ = io::stdout().flush();
        } else {
            println!("[{idx}/{total}] {label}");
        }

        let cfg = ProcessConfig {
            output_dir: output_dir.clone(),
            input_root: job.input_root.clone(),
            powershell: powershell.clone(),
            verbose: cli.verbose,
            dry_run: cli.dry_run,
        };

        let result = if failed_outputs
            .iter()
            .any(|dest: &PathBuf| same_path(dest, &job.dest))
        {
            Err(anyhow!(
                "输出路径已有失败任务的文件，为保留该文件而跳过: {}",
                job.dest.display()
            ))
        } else {
            process_file(&job.source, &cfg)
        };
        match result {
            Ok(outcome) => {
                ok += 1;
                print_success(&outcome, cli.verbose, cli.dry_run);
            }
            Err(err) => {
                fail += 1;
                failed_outputs.push(job.dest.clone());
                if !cli.verbose {
                    println!("失败");
                }
                eprintln!("    {err:#}");
                if cli.fail_fast {
                    bail!("已启用 --fail-fast，在第 {idx} 个文件处停止");
                }
            }
        }
    }

    if cli.dry_run {
        println!(
            "[dry-run] 全部成功后删除空输出目录: {}",
            output_dir.display()
        );
    } else if fail == 0 {
        if cleanup_output_dir(&output_dir)? {
            if cli.verbose {
                println!("已删除输出目录: {}", output_dir.display());
            }
        } else {
            eprintln!("输出目录仍有文件或链接，已保留: {}", output_dir.display());
        }
    } else {
        eprintln!(
            "存在处理失败的文件，已保留输出目录: {}",
            output_dir.display()
        );
    }

    println!();
    println!(
        "完成: 成功 {ok}, 失败 {fail}, 合计 {total}{}",
        if cli.dry_run { " (dry-run)" } else { "" }
    );

    if fail > 0 {
        Ok(ExitCode::from(1))
    } else {
        Ok(ExitCode::SUCCESS)
    }
}

struct Job {
    source: PathBuf,
    dest: PathBuf,
    input_root: Option<PathBuf>,
}

fn resolve_paths(cli: &Cli) -> Result<Vec<PathBuf>> {
    if !cli.paths.is_empty() {
        return Ok(cli.paths.iter().map(|p| pathutil::normalize(p)).collect());
    }
    eprint!("请输入文件或目录路径: ");
    io::stderr().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("读取交互输入失败")?;
    let line = line.trim().trim_matches('"').trim_matches('\'').trim();
    if line.is_empty() {
        bail!("未输入路径");
    }
    Ok(vec![pathutil::normalize(Path::new(line))])
}

fn resolve_output_dir(cli: &Cli, paths: &[PathBuf]) -> PathBuf {
    if cli.output == Path::new("output")
        && let Some(first) = paths.first()
        && should_relocate_default_output()
    {
        let first_abs = absolute(first).unwrap_or_else(|_| first.clone());
        let base = if first_abs.is_dir() {
            first_abs
        } else {
            first_abs
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(first_abs)
        };
        return base.join("output");
    }
    absolute(&cli.output).unwrap_or_else(|_| cli.output.clone())
}

fn should_relocate_default_output() -> bool {
    let Ok(cwd) = std::env::current_dir() else {
        return true;
    };
    if let Some(sys_root) = std::env::var_os("SystemRoot") {
        let sys_path = PathBuf::from(sys_root);
        if pathutil::is_within(&cwd, &sys_path) {
            return true;
        }
    }
    false
}

fn display_source(source: &Path, input_root: Option<&Path>) -> String {
    if let Some(root) = input_root
        && let Some(rel) = strip_prefix_ci(source, root)
        && !rel.as_os_str().is_empty()
    {
        return rel.display().to_string();
    }
    source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source.display().to_string())
}

fn print_success(outcome: &FileOutcome, verbose: bool, dry_run: bool) {
    let tag = if dry_run {
        "将替换源文件"
    } else {
        "完成"
    };
    if verbose {
        println!(
            "    {tag}: {} ({} 字节)",
            outcome.dest.display(),
            outcome.bytes
        );
    } else {
        println!(
            "{tag} -> {} ({} 字节)",
            outcome.dest.display(),
            outcome.bytes
        );
    }
}

fn warn_destination_collisions(jobs: &[Job]) {
    let mut by_dest: HashMap<PathBuf, Vec<&Path>> = HashMap::new();
    for job in jobs {
        by_dest
            .entry(job.dest.clone())
            .or_default()
            .push(&job.source);
    }
    for (dest, sources) in by_dest {
        if sources.len() > 1 {
            eprintln!("警告: 多个源文件将写入同一目标 {}:", dest.display());
            for s in sources {
                eprintln!("    - {}", s.display());
            }
        }
    }
}

#[cfg(windows)]
mod win_console {
    unsafe extern "system" {
        pub fn SetConsoleOutputCP(w_code_page_id: u32) -> i32;
        pub fn SetConsoleCP(w_code_page_id: u32) -> i32;
    }
}

#[cfg(windows)]
fn enable_windows_utf8() {
    const UTF8: u32 = 65001;
    unsafe {
        win_console::SetConsoleOutputCP(UTF8);
        win_console::SetConsoleCP(UTF8);
    }
}

#[cfg(not(windows))]
fn enable_windows_utf8() {}
