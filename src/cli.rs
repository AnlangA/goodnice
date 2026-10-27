use std::path::PathBuf;

use clap::Parser;

/// 高速透明解密工具：零拷贝硬链接映射 + 内存流式剥离加密壳 + 原生二进制管道校验落盘。
///
/// 核心架构：
///   1. 零拷贝硬链接映射：非 .xlsx 文件优先通过同卷隐藏区 NTFS 硬链接瞬间映射为 .xlsx 句柄（0 字节拷贝）；
///   2. 智能快速跳过：对未加密大文件毫秒级识别并直接跳过，避免无效 I/O；
///   3. 内存递归剥壳：由具备亿赛通表格解密权限的 python.exe 将字节流读入内存，若存在多层嵌套加密则自动递归剥离；
///   4. 原生二进制管道落盘：通过独立进程名的原生工作进程（dot-worker.exe）完成校验并一次性落盘。
///
/// PATH 为文件时解密该文件；为目录时默认递归解密该目录及其所有子目录中的文件。
#[derive(Parser, Debug)]
#[command(
    name = "dot",
    version,
    about,
    long_about = None,
    after_help = "示例:\n  \
        dot file.pptx                解密单个文件\n  \
        dot ./docs                   默认递归解密目录及所有子目录中的文件\n  \
        dot ./docs --no-recursive    仅解密当前目录顶层文件（不进入子目录）\n  \
        dot ./docs -e pdf -e xlsx    仅处理 pdf 与 xlsx\n  \
        dot ./docs --dry-run         预览将要处理的文件\n",
    next_line_help = true
)]
pub struct Cli {
    /// 要处理的文件或目录。可指定多个。省略时在终端交互输入。
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,

    /// 递归处理目录下所有子目录中的文件（默认对目录已开启递归，保留此参数兼容旧命令）
    #[arg(short, long)]
    pub recursive: bool,

    /// 仅处理目录顶层文件，不递归进入子目录
    #[arg(long = "no-recursive")]
    pub no_recursive: bool,

    /// 回退输出目录（仅当源文件被外部程序锁死无法替换时使用，默认：output/）
    #[arg(short, long, value_name = "DIR", default_value = "output")]
    pub output: PathBuf,

    /// 仅处理指定扩展名的文件，可重复。例如: -e pdf -e xlsx
    #[arg(short, long, value_name = "EXT", action = clap::ArgAction::Append)]
    pub extension: Vec<String>,

    /// 额外跳过的目录名，可重复。默认已跳过 .git、target、node_modules 等
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

    /// 静默模式，不输出常规进度日志（供右键静默解密或脚本调用）
    #[arg(short, long)]
    pub quiet: bool,

    /// 内部原生 I/O 工作进程模式（隐藏参数）
    #[arg(long = "internal-io-worker", hide = true)]
    pub internal_io_worker: bool,
}

impl Cli {
    /// 目录是否启用递归遍历（默认开启，除非显式传入 `--no-recursive`）
    pub fn is_recursive(&self) -> bool {
        !self.no_recursive
    }
}
