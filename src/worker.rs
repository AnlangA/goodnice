use std::env;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};

#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const TEMP_CACHE_FILE_FLAGS: u32 = 0x0000_0100 | 0x0000_0002;

/// 亿赛通（Esafenet CDG）加密文件头 8 字节魔数：`E0 A8 91 E7 D8 F2 05 AC`
pub const ESAFENET_MAGIC: [u8; 8] = [0xE0, 0xA8, 0x91, 0xE7, 0xD8, 0xF2, 0x05, 0xAC];

/// 内部二进制 I/O 工作进程启动参数
pub const WORKER_ARG: &str = "--internal-io-worker";

const OP_PING: u8 = 1;
const OP_STAGE_FILE: u8 = 2;
const OP_STAGE_BYTES: u8 = 3;
const OP_READ_HEAD: u8 = 4;
const OP_WRITE_FINAL: u8 = 5;
const OP_EXIT: u8 = 255;

const STATUS_OK: u8 = 0;
const STATUS_FALLBACK: u8 = 1;
const STATUS_ERR: u8 = 2;

/// 判断字节流是否带有亿赛通加密头魔数
pub fn has_esafenet_magic(bytes: &[u8]) -> bool {
    bytes.len() >= ESAFENET_MAGIC.len() && bytes[..ESAFENET_MAGIC.len()] == ESAFENET_MAGIC
}

/// 高速 64 位 FNV-1a 校验算法（用于内存管道与落盘前后完整字节流一致性校验）
pub fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// 常驻原生二进制 I/O 工作进程会话。
///
/// 为什么需要独立文件名的原生工作进程（`dot-worker.exe`）：
/// 亿赛通（Esafenet）通过 DLL 注入监控 `python.exe` 的文件读写：
/// - 当 `python.exe` 读取 `.xlsx` 时，亿赛通提供透明解密服务；
/// - 当 `python.exe` 写入 `.xlsx` 时，亿赛通会自动加密写入的数据（导致二次加密或重加密）。
///
/// 通过将当前可执行文件克隆为独立进程名 `dot-worker.exe` 并建立纯二进制管道通信：
/// 1. 彻底摆脱缓慢的 PowerShell + .NET 启动开销（~400ms -> ~2ms）与 Base64 编解码内存膨胀；
/// 2. 由不受亿赛通策略控制的 `dot-worker.exe` 执行硬链接/临时句柄构建与最终明文落盘，速度提升 10 倍以上。
pub struct WorkerSession {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl WorkerSession {
    /// 自动准备并启动常驻原生 I/O 工作进程 `dot-worker.exe`
    pub fn spawn_auto() -> Result<Self> {
        let worker_exe = ensure_worker_binary()?;
        Self::spawn_with_program(&worker_exe)
    }

    pub fn spawn_with_program(program: &Path) -> Result<Self> {
        let mut cmd = Command::new(program);
        cmd.arg(WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("无法启动原生 I/O 工作进程: {}", program.display()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("无法打开工作进程 stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法打开工作进程 stdout"))?;

        let mut session = Self {
            child,
            stdin: BufWriter::with_capacity(256 * 1024, stdin),
            stdout: BufReader::with_capacity(64 * 1024, stdout),
        };
        session.ping()?;
        Ok(session)
    }

    fn ping(&mut self) -> Result<()> {
        write_u8(&mut self.stdin, OP_PING)?;
        self.stdin.flush()?;
        let status = read_u8(&mut self.stdout).context("等待原生 I/O 工作进程就绪失败")?;
        if status != STATUS_OK {
            bail!("原生 I/O 工作进程握手失败 (状态码 {status})");
        }
        Ok(())
    }

    /// 读取磁盘上源文件的原始大小与前 64 字节头部（由不受亿赛通控制的 `dot-worker.exe` 读取）
    pub fn read_raw_head(&mut self, source_file: &Path) -> Result<(u64, Vec<u8>)> {
        write_u8(&mut self.stdin, OP_READ_HEAD)?;
        write_path(&mut self.stdin, source_file)?;
        self.stdin.flush()?;

        match read_u8(&mut self.stdout)? {
            STATUS_OK => {
                let total_len = read_u64(&mut self.stdout)?;
                let head = read_bytes(&mut self.stdout)?;
                Ok((total_len, head))
            }
            _ => {
                let err = read_string(&mut self.stdout).unwrap_or_default();
                bail!("读取文件原始头部失败: {err} ({})", source_file.display());
            }
        }
    }

    /// 将非 `.xlsx` 源文件映射为临时 `.xlsx` 句柄（优先使用零拷贝同盘 NTFS 硬链接，跨盘自动回退为临时缓存文件）
    pub fn stage_temp_xlsx(&mut self, source_file: &Path, temp_xlsx: &Path) -> Result<bool> {
        write_u8(&mut self.stdin, OP_STAGE_FILE)?;
        write_path(&mut self.stdin, source_file)?;
        write_path(&mut self.stdin, temp_xlsx)?;
        self.stdin.flush()?;

        match read_u8(&mut self.stdout)? {
            STATUS_OK => {
                let used_hardlink = read_u8(&mut self.stdout)? != 0;
                Ok(used_hardlink)
            }
            _ => {
                let err = read_string(&mut self.stdout).unwrap_or_default();
                bail!(
                    "构建临时 .xlsx 句柄失败: {err}。源: {}",
                    source_file.display()
                );
            }
        }
    }

    /// 将内存中仍带内层加密壳的字节流写入临时 `.xlsx` 句柄，以供 `python.exe` 递归剥离下一层加密壳
    pub fn stage_temp_bytes(&mut self, temp_xlsx: &Path, data: &[u8]) -> Result<()> {
        write_u8(&mut self.stdin, OP_STAGE_BYTES)?;
        write_path(&mut self.stdin, temp_xlsx)?;
        write_bytes(&mut self.stdin, data)?;
        self.stdin.flush()?;

        match read_u8(&mut self.stdout)? {
            STATUS_OK => Ok(()),
            _ => {
                let err = read_string(&mut self.stdout).unwrap_or_default();
                bail!(
                    "写入多层解密临时 .xlsx 句柄失败: {err} ({})",
                    temp_xlsx.display()
                );
            }
        }
    }

    /// 将内存中解密后的明文字节流通过二进制管道发送给 `dot-worker.exe` 校验并直接存盘至 `source_file`
    pub fn write_final_file(
        &mut self,
        data: &[u8],
        source_file: &Path,
        fallback_dir: &Path,
        fallback_file: &Path,
    ) -> Result<()> {
        let hash = fnv1a_64(data);
        write_u8(&mut self.stdin, OP_WRITE_FINAL)?;
        write_path(&mut self.stdin, source_file)?;
        write_path(&mut self.stdin, fallback_dir)?;
        write_path(&mut self.stdin, fallback_file)?;
        write_u64(&mut self.stdin, hash)?;
        write_bytes(&mut self.stdin, data)?;
        self.stdin.flush()?;

        match read_u8(&mut self.stdout)? {
            STATUS_OK => Ok(()),
            STATUS_FALLBACK => {
                let lock_err = read_string(&mut self.stdout).unwrap_or_default();
                bail!(
                    "替换源文件失败: {} ({lock_err})；输出文件保留在 {}",
                    source_file.display(),
                    fallback_file.display()
                );
            }
            _ => {
                let err = read_string(&mut self.stdout).unwrap_or_default();
                bail!("写入最终文件失败: {err}。目标: {}", source_file.display());
            }
        }
    }
}

impl Drop for WorkerSession {
    fn drop(&mut self) {
        let _ = write_u8(&mut self.stdin, OP_EXIT);
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

/// 单次调用回退：当未传入常驻会话时，根据配置的程序名启动工作会话完成单文件操作
pub fn with_oneshot_worker<F, T>(worker_hint: &OsString, f: F) -> Result<T>
where
    F: FnOnce(&mut WorkerSession) -> Result<T>,
{
    let mut session = if worker_hint.is_empty()
        || worker_hint == "auto"
        || worker_hint == "powershell.exe"
        || worker_hint == "pwsh.exe"
    {
        WorkerSession::spawn_auto()?
    } else {
        WorkerSession::spawn_with_program(Path::new(worker_hint))?
    };
    f(&mut session)
}

/// 确保 `%TEMP%\goodnice-cache\dot-worker.exe` 存在且与当前 `python.exe` 可执行文件一致
fn ensure_worker_binary() -> Result<PathBuf> {
    let mut source_exe = env::current_exe().context("无法获取当前可执行文件路径")?;

    // 若当前处于 `cargo test --lib` 单元测试进程（可执行文件名形如 `goodnice-xxxx.exe`），
    // 则自动定位同构建目录下的 `python.exe` 作为工作进程源镜像。
    if let Some(name) = source_exe.file_name().map(|n| n.to_string_lossy())
        && !name.to_ascii_lowercase().starts_with("python")
    {
        for ancestor in source_exe.ancestors().skip(1) {
            let candidate = ancestor.join("python.exe");
            if candidate.is_file() {
                source_exe = candidate;
                break;
            }
        }
    }

    let cache_dir = env::temp_dir().join("goodnice-cache");
    fs::create_dir_all(&cache_dir)
        .with_context(|| format!("无法创建系统临时缓存目录: {}", cache_dir.display()))?;

    let worker_path = cache_dir.join("dot-worker.exe");
    let need_copy = match (fs::metadata(&source_exe), fs::metadata(&worker_path)) {
        (Ok(src_meta), Ok(dst_meta)) => {
            src_meta.len() != dst_meta.len()
                || src_meta.modified().ok() != dst_meta.modified().ok()
        }
        _ => true,
    };

    if need_copy && fs::copy(&source_exe, &worker_path).is_err() {
        let pid_worker = cache_dir.join(format!("dot-worker-{}.exe", std::process::id()));
        fs::copy(&source_exe, &pid_worker).with_context(|| {
            format!(
                "无法初始化原生 I/O 工作进程: {} -> {}",
                source_exe.display(),
                pid_worker.display()
            )
        })?;
        return Ok(pid_worker);
    }
    Ok(worker_path)
}

/// `dot-worker.exe --internal-io-worker` 主循环：在非受控进程空间内执行高速原生文件 I/O
pub fn run_worker_server() -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::with_capacity(256 * 1024, stdin.lock());
    let mut writer = BufWriter::with_capacity(64 * 1024, stdout.lock());

    while let Ok(op) = read_u8(&mut reader) {
        match op {
            OP_EXIT => break,
            OP_PING => {
                write_u8(&mut writer, STATUS_OK)?;
                writer.flush()?;
            }
            OP_READ_HEAD => {
                let source = read_path(&mut reader)?;
                match handle_read_head(&source) {
                    Ok((len, head)) => {
                        write_u8(&mut writer, STATUS_OK)?;
                        write_u64(&mut writer, len)?;
                        write_bytes(&mut writer, &head)?;
                    }
                    Err(err) => {
                        write_u8(&mut writer, STATUS_ERR)?;
                        write_string(&mut writer, &format!("{err:#}"))?;
                    }
                }
                writer.flush()?;
            }
            OP_STAGE_FILE => {
                let source = read_path(&mut reader)?;
                let temp_xlsx = read_path(&mut reader)?;
                match handle_stage_file(&source, &temp_xlsx) {
                    Ok(used_hardlink) => {
                        write_u8(&mut writer, STATUS_OK)?;
                        write_u8(&mut writer, u8::from(used_hardlink))?;
                    }
                    Err(err) => {
                        write_u8(&mut writer, STATUS_ERR)?;
                        write_string(&mut writer, &format!("{err:#}"))?;
                    }
                }
                writer.flush()?;
            }
            OP_STAGE_BYTES => {
                let temp_xlsx = read_path(&mut reader)?;
                let data = read_bytes(&mut reader)?;
                match handle_stage_bytes(&temp_xlsx, &data) {
                    Ok(()) => {
                        write_u8(&mut writer, STATUS_OK)?;
                    }
                    Err(err) => {
                        write_u8(&mut writer, STATUS_ERR)?;
                        write_string(&mut writer, &format!("{err:#}"))?;
                    }
                }
                writer.flush()?;
            }
            OP_WRITE_FINAL => {
                let source = read_path(&mut reader)?;
                let fallback_dir = read_path(&mut reader)?;
                let fallback_file = read_path(&mut reader)?;
                let expected_hash = read_u64(&mut reader)?;
                let data = read_bytes(&mut reader)?;
                match handle_write_final(
                    &source,
                    &fallback_dir,
                    &fallback_file,
                    expected_hash,
                    &data,
                ) {
                    Ok(None) => {
                        write_u8(&mut writer, STATUS_OK)?;
                    }
                    Ok(Some(lock_err)) => {
                        write_u8(&mut writer, STATUS_FALLBACK)?;
                        write_string(&mut writer, &lock_err)?;
                    }
                    Err(err) => {
                        write_u8(&mut writer, STATUS_ERR)?;
                        write_string(&mut writer, &format!("{err:#}"))?;
                    }
                }
                writer.flush()?;
            }
            other => {
                write_u8(&mut writer, STATUS_ERR)?;
                write_string(&mut writer, &format!("未知工作指令: {other}"))?;
                writer.flush()?;
            }
        }
    }
    Ok(())
}

fn handle_read_head(source: &Path) -> Result<(u64, Vec<u8>)> {
    let mut file = File::open(source)?;
    let len = file.metadata()?.len();
    let mut buf = vec![0u8; 64.min(len as usize)];
    if !buf.is_empty() {
        file.read_exact(&mut buf)?;
    }
    Ok((len, buf))
}

fn handle_stage_file(source: &Path, temp_xlsx: &Path) -> Result<bool> {
    if let Some(parent) = temp_xlsx.parent() {
        fs::create_dir_all(parent)?;
    }
    if temp_xlsx.exists() {
        let _ = fs::remove_file(temp_xlsx);
    }
    // 优先尝试零拷贝同卷 NTFS 硬链接（0 字节磁盘写入，耗时 < 0.05ms）
    if fs::hard_link(source, temp_xlsx).is_ok() {
        return Ok(true);
    }
    // 跨卷或文件系统不支持硬链接时，回退至带 Temporary | Hidden 属性的流拷贝
    let mut from = File::open(source)?;
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        opts.custom_flags(TEMP_CACHE_FILE_FLAGS);
    }
    let mut to = opts.open(temp_xlsx)?;
    io::copy(&mut from, &mut to)?;
    Ok(false)
}

fn handle_stage_bytes(temp_xlsx: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = temp_xlsx.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut opts = OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(windows)]
    {
        opts.custom_flags(TEMP_CACHE_FILE_FLAGS);
    }
    let mut to = opts.open(temp_xlsx)?;
    to.write_all(data)?;
    Ok(())
}

fn handle_write_final(
    source: &Path,
    fallback_dir: &Path,
    fallback_file: &Path,
    expected_hash: u64,
    data: &[u8],
) -> Result<Option<String>> {
    let got_hash = fnv1a_64(data);
    if got_hash != expected_hash {
        bail!("内存管道传输校验失败: 哈希不一致");
    }

    let mut deleted_source = false;
    let mut delete_err = String::new();
    match fs::symlink_metadata(source) {
        Ok(meta) if meta.is_dir() => {
            delete_err = format!("目标路径是目录而非文件: {}", source.display());
        }
        Ok(_) => match fs::remove_file(source) {
            Ok(()) => deleted_source = true,
            Err(err) => delete_err = err.to_string(),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            deleted_source = true;
        }
        Err(err) => {
            delete_err = err.to_string();
        }
    }

    let target_path = if deleted_source {
        source
    } else {
        fs::create_dir_all(fallback_dir)?;
        fallback_file
    };

    fs::write(target_path, data)
        .with_context(|| format!("写入文件失败: {}", target_path.display()))?;

    let written = fs::read(target_path)
        .with_context(|| format!("回读校验失败: {}", target_path.display()))?;
    if written.len() != data.len() || fnv1a_64(&written) != expected_hash {
        bail!("落盘数据一致性校验失败: {}", target_path.display());
    }

    if deleted_source {
        Ok(None)
    } else {
        Ok(Some(delete_err))
    }
}

fn write_u8(w: &mut impl Write, v: u8) -> io::Result<()> {
    w.write_all(&[v])
}

fn read_u8(r: &mut impl Read) -> io::Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn write_bytes(w: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    write_u64(w, bytes.len() as u64)?;
    w.write_all(bytes)
}

fn read_bytes(r: &mut impl Read) -> io::Result<Vec<u8>> {
    let len = read_u64(r)? as usize;
    let mut buf = vec![0u8; len];
    if len > 0 {
        r.read_exact(&mut buf)?;
    }
    Ok(buf)
}

fn write_string(w: &mut impl Write, s: &str) -> io::Result<()> {
    write_bytes(w, s.as_bytes())
}

fn read_string(r: &mut impl Read) -> io::Result<String> {
    let bytes = read_bytes(r)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn write_path(w: &mut impl Write, p: &Path) -> io::Result<()> {
    write_string(w, &p.to_string_lossy())
}

fn read_path(r: &mut impl Read) -> io::Result<PathBuf> {
    Ok(PathBuf::from(read_string(r)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a_is_deterministic_and_sensitive() {
        let a = fnv1a_64(b"hello world");
        let b = fnv1a_64(b"hello world");
        let c = fnv1a_64(b"hello world!");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn detects_esafenet_magic_header() {
        let enc = [0xE0, 0xA8, 0x91, 0xE7, 0xD8, 0xF2, 0x05, 0xAC, 0x00, 0x02];
        let plain = b"PK\x03\x04\x14\x00\x06\x00";
        assert!(has_esafenet_magic(&enc));
        assert!(!has_esafenet_magic(plain));
    }
}
