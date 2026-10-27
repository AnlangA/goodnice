use std::path::PathBuf;

use clap::Parser;

/// 读取文件原始字节流，经 PowerShell 写入并校验后替换源文件。
///
/// 处理流程：
///   1. 非 xlsx 文件会先复制一份并把后缀改为 .xlsx（原文件保留）；
///   2. 用本程序读取字节流，完全通过 PowerShell 写入输出目录；
///   3. 若第 1 步改过格式，则把输出文件改回原本后缀，并删除中间 xlsx 副本。
///   4. 以上步骤成功后，删除源文件；
///   5. 将输出文件移回源文件所在目录，保留原文件名；
///   6. 全部文件处理成功后，删除已清空的输出目录及其子目录。
///
/// PATH 为文件时处理该文件；为目录时处理该目录下的文件。
/// 加上 `-r/--recursive` 后，会继续递归处理所有子目录中的文件。
#[derive(Parser, Debug)]
#[command(
    name = "dot",
    version,
    about,
    long_about = None,
    after_help = "示例:\n  \
        dot file.pdf                 处理单个文件\n  \
        dot ./docs                   处理目录中的文件（不进入子目录）\n  \
        dot ./docs -r                递归处理目录及所有子目录中的文件\n  \
        dot ./docs -r -e pdf -e xlsx 仅处理 pdf 与 xlsx\n  \
        dot a.pdf b.pdf ./docs -o out\n  \
        dot ./docs -r --dry-run      预览将要处理的文件\n",
    next_line_help = true
)]
pub struct Cli {
    /// 要处理的文件或目录。可指定多个。省略时在终端交互输入。
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// 递归处理目录下所有子目录中的文件（仅对目录生效）
    #[arg(short, long)]
    pub recursive: bool,

    /// 临时输出目录，成功后移回源目录并清理空目录（默认：output/）
    #[arg(short, long, value_name = "DIR", default_value = "output")]
    pub output: PathBuf,

    /// 仅处理指定扩展名的文件，可重复。例如: -e pdf -e xlsx
    #[arg(short, long, value_name = "EXT", action = clap::ArgAction::Append)]
    pub extension: Vec<String>,

    /// 额外跳过的目录名，可重复。默认已跳过 .git、target、node_modules
    #[arg(long = "exclude-dir", value_name = "NAME", action = clap::ArgAction::Append)]
    pub exclude_dir: Vec<String>,

    /// 只预览处理和清理步骤，不读取文件内容、不写入、不删除
    #[arg(long)]
    pub dry_run: bool,

    /// 遇到第一个错误即停止（默认：记录错误并继续处理其余文件）
    #[arg(long)]
    pub fail_fast: bool,

    /// 显示每一步的详细日志
    #[arg(short, long)]
    pub verbose: bool,
}
