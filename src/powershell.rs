use std::ffi::OsString;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

/// 隐藏 PowerShell 子进程控制台，避免进度/CLIXML 泄漏到当前终端。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn powershell_command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// 单次调用脚本（供单文件或无常驻会话回退使用）：
/// 字节流在内存中解码并计算 MD5，处理完成后直接将最终文件存盘至源文件位置（若源文件被锁无法替换则回退保存至输出目录）。
const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$b64 = [Console]::In.ReadToEnd().Trim()
$bytes = [Convert]::FromBase64String($b64)

$sourceFile = $env:GOODNICE_SOURCE_FILE
$outDir = $env:GOODNICE_OUT_DIR
$outFile = $env:GOODNICE_OUT_FILE
if ([string]::IsNullOrWhiteSpace($sourceFile) -or [string]::IsNullOrWhiteSpace($outDir) -or [string]::IsNullOrWhiteSpace($outFile)) {
    throw '内部错误: 未设置 GOODNICE_SOURCE_FILE / GOODNICE_OUT_DIR / GOODNICE_OUT_FILE'
}

$md5 = [System.Security.Cryptography.MD5]::Create()
$h1 = $md5.ComputeHash($bytes)

$deletedSource = $false
$deleteErr = ''
try {
    if ([System.IO.File]::Exists($sourceFile)) {
        [System.IO.File]::Delete($sourceFile)
    } elseif ([System.IO.Directory]::Exists($sourceFile)) {
        throw "目标路径是目录而非文件: $sourceFile"
    }
    $deletedSource = $true
} catch {
    $deleteErr = $_.Exception.Message -replace '[\r\n]+', ' '
}

$targetPath = if ($deletedSource) { $sourceFile } else {
    if (-not [System.IO.Directory]::Exists($outDir)) {
        [System.IO.Directory]::CreateDirectory($outDir) | Out-Null
    }
    $outFile
}

[System.IO.File]::WriteAllBytes($targetPath, $bytes)
$written = [System.IO.File]::ReadAllBytes($targetPath)
if ($bytes.Length -ne $written.Length) {
    throw '验证失败: 写入文件的长度与接收的数据不一致!'
}
$h2 = $md5.ComputeHash($written)
for ($i = 0; $i -lt $h1.Length; $i++) {
    if ($h1[$i] -ne $h2[$i]) {
        throw '验证失败: 写入文件的字节流与接收的数据不一致!'
    }
}

if (-not $deletedSource) {
    throw "替换源文件失败: $sourceFile ($deleteErr)；输出文件保留在 $outFile"
}

if ($env:GOODNICE_VERBOSE -eq '1') {
    $fi = Get-Item -LiteralPath $targetPath
    [Console]::Out.WriteLine('PowerShell written: ' + $fi.FullName + ' (' + $fi.Length + ' bytes)')
    [Console]::Out.WriteLine('PowerShell 验证通过: 内存字节流与最终存盘数据完全一致')
}
"#;

/// 常驻 PowerShell 工作进程脚本：
/// 全程在内存中接收字节流并计算哈希，处理完成后直接将最终文件写入目标盘位（无需在磁盘创建临时输出目录和中间输出文件）。
const PS_WORKER_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$utf8 = [System.Text.UTF8Encoding]::new($false)
$md5 = [System.Security.Cryptography.MD5]::Create()
[Console]::Out.WriteLine('GOODNICE_READY')
[Console]::Out.Flush()

while ($true) {
    $header = [Console]::In.ReadLine()
    if ($null -eq $header -or $header -eq 'EXIT') { break }
    try {
        $parts = $header.Split('|', 4)
        if ($parts.Length -lt 4) {
            throw '内部协议错误: 请求头格式无效'
        }
        $verbose = $parts[0]
        $sourceFile = $utf8.GetString([Convert]::FromBase64String($parts[1]))
        $outDir = $utf8.GetString([Convert]::FromBase64String($parts[2]))
        $outFile = $utf8.GetString([Convert]::FromBase64String($parts[3]))
        $b64 = [Console]::In.ReadLine()
        if ($null -eq $b64) {
            throw '内部协议错误: 未收到文件字节流负载'
        }
        $bytes = [Convert]::FromBase64String($b64)
        $h1 = $md5.ComputeHash($bytes)

        $deletedSource = $false
        $deleteErr = ''
        try {
            if ([System.IO.File]::Exists($sourceFile)) {
                [System.IO.File]::Delete($sourceFile)
            } elseif ([System.IO.Directory]::Exists($sourceFile)) {
                throw "目标路径是目录而非文件: $sourceFile"
            }
            $deletedSource = $true
        } catch {
            $deleteErr = $_.Exception.Message -replace '[\r\n]+', ' '
        }

        $targetPath = if ($deletedSource) {
            $sourceFile
        } else {
            if (-not [System.IO.Directory]::Exists($outDir)) {
                [System.IO.Directory]::CreateDirectory($outDir) | Out-Null
            }
            $outFile
        }

        [System.IO.File]::WriteAllBytes($targetPath, $bytes)
        $written = [System.IO.File]::ReadAllBytes($targetPath)
        if ($bytes.Length -ne $written.Length) {
            throw '验证失败: 写入文件的长度与接收的数据不一致!'
        }
        $h2 = $md5.ComputeHash($written)
        for ($i = 0; $i -lt $h1.Length; $i++) {
            if ($h1[$i] -ne $h2[$i]) {
                throw '验证失败: 写入文件的字节流与接收的数据不一致!'
            }
        }

        if (-not $deletedSource) {
            [Console]::Out.WriteLine('GOODNICE_FALLBACK|' + $deleteErr)
            [Console]::Out.Flush()
            continue
        }

        if ($verbose -eq '1') {
            [Console]::Out.WriteLine('GOODNICE_LOG|PowerShell written: ' + $targetPath + ' (' + $written.Length + ' bytes)')
            [Console]::Out.WriteLine('GOODNICE_LOG|PowerShell 验证通过: 内存字节流与最终存盘数据完全一致')
        }
        [Console]::Out.WriteLine('GOODNICE_OK')
        [Console]::Out.Flush()
    } catch {
        $errMsg = $_.Exception.Message -replace '[\r\n]+', ' '
        [Console]::Out.WriteLine('GOODNICE_ERR|' + $errMsg)
        [Console]::Out.Flush()
    }
}
"#;

const CANDIDATES: &[&str] = &["powershell.exe", "pwsh.exe", "powershell", "pwsh"];

/// 常驻 PowerShell 写入与校验会话，在批量处理时消除重复启动 PowerShell 子进程的开销。
pub struct PowerShellSession {
    program: OsString,
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl PowerShellSession {
    /// 自动探测可用 PowerShell 并直接启动常驻工作进程（同时完成可用性校验与预热）。
    pub fn spawn_auto() -> Result<Self> {
        let mut last_err = None;
        for prog in CANDIDATES {
            let os_prog = OsString::from(prog);
            match Self::spawn(os_prog) {
                Ok(session) => return Ok(session),
                Err(err) => last_err = Some(err),
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow!("未找到 PowerShell"))
            .context("未找到可用的 PowerShell（powershell.exe / pwsh）。写入必须由 PowerShell 完成"))
    }

    pub fn spawn(program: OsString) -> Result<Self> {
        let encoded = encode_command(PS_WORKER_SCRIPT);
        let mut child = powershell_command(&program)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-NoLogo",
                "-EncodedCommand",
                &encoded,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("无法启动 PowerShell: {}", program.to_string_lossy()))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("无法打开 PowerShell stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("无法打开 PowerShell stdout"))?;

        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader
                .read_line(&mut line)
                .context("等待 PowerShell 工作进程就绪失败")?;
            if n == 0 {
                let status = child.wait().ok();
                bail!(
                    "PowerShell 工作进程启动失败 (退出码 {:?})",
                    status.and_then(|s| s.code())
                );
            }
            if line.trim() == "GOODNICE_READY" {
                break;
            }
        }

        Ok(Self {
            program,
            child,
            stdin: BufWriter::with_capacity(64 * 1024, stdin),
            stdout: reader,
        })
    }

    pub fn program(&self) -> &OsString {
        &self.program
    }

    /// 将内存中的字节流直接通过常驻 PowerShell 校验并存盘至 `source_file`（若源文件被锁无法替换则保存至 `fallback_file`）。
    pub fn write_final_file(
        &mut self,
        data: &[u8],
        source_file: &Path,
        fallback_dir: &Path,
        fallback_file: &Path,
        verbose: bool,
    ) -> Result<()> {
        let src_b64 = STANDARD.encode(source_file.to_string_lossy().as_bytes());
        let dir_b64 = STANDARD.encode(fallback_dir.to_string_lossy().as_bytes());
        let file_b64 = STANDARD.encode(fallback_file.to_string_lossy().as_bytes());
        let v_flag = if verbose { "1" } else { "0" };

        writeln!(self.stdin, "{v_flag}|{src_b64}|{dir_b64}|{file_b64}")
            .context("向 PowerShell 会话发送请求头失败")?;
        let payload = STANDARD.encode(data);
        self.stdin
            .write_all(payload.as_bytes())
            .context("向 PowerShell 会话写入字节流失败")?;
        self.stdin
            .write_all(b"\n")
            .context("向 PowerShell 会话写入换行符失败")?;
        self.stdin.flush().context("刷新 PowerShell 会话输入流失败")?;

        let mut line = String::new();
        loop {
            line.clear();
            let n = self
                .stdout
                .read_line(&mut line)
                .context("读取 PowerShell 会话响应失败")?;
            if n == 0 {
                bail!(
                    "PowerShell 会话意外终止。目标: {}",
                    source_file.display()
                );
            }
            let trimmed = line.trim();
            if let Some(log_msg) = trimmed.strip_prefix("GOODNICE_LOG|") {
                if verbose {
                    println!("    {log_msg}");
                }
            } else if trimmed == "GOODNICE_OK" {
                return Ok(());
            } else if let Some(lock_err) = trimmed.strip_prefix("GOODNICE_FALLBACK|") {
                bail!(
                    "替换源文件失败: {} ({lock_err})；输出文件保留在 {}",
                    source_file.display(),
                    fallback_file.display()
                );
            } else if let Some(err_msg) = trimmed.strip_prefix("GOODNICE_ERR|") {
                bail!(
                    "PowerShell 写入失败: {err_msg}。目标: {}",
                    source_file.display()
                );
            }
        }
    }
}

impl Drop for PowerShellSession {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "EXIT");
        let _ = self.stdin.flush();
        let _ = self.child.wait();
    }
}

fn encode_command(script: &str) -> String {
    let utf16: Vec<u16> = script.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for unit in utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    STANDARD.encode(bytes)
}

/// 把内存中的原始字节流交给单次 PowerShell 进程，直接写入最终目标文件并校验 MD5。
pub fn write_bytes_via_powershell(
    powershell: &OsString,
    data: &[u8],
    source_file: &Path,
    fallback_dir: &Path,
    fallback_file: &Path,
    verbose: bool,
) -> Result<()> {
    let encoded = encode_command(PS_SCRIPT);
    let payload = STANDARD.encode(data);

    let mut child = powershell_command(powershell)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-NoLogo",
            "-EncodedCommand",
            &encoded,
        ])
        .env("GOODNICE_SOURCE_FILE", source_file.as_os_str())
        .env("GOODNICE_OUT_DIR", fallback_dir.as_os_str())
        .env("GOODNICE_OUT_FILE", fallback_file.as_os_str())
        .env("GOODNICE_VERBOSE", if verbose { "1" } else { "0" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("无法启动 PowerShell: {}", powershell.to_string_lossy()))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("无法打开 PowerShell stdin"))?;
        stdin
            .write_all(payload.as_bytes())
            .context("向 PowerShell 写入字节流失败")?;
        stdin.flush().ok();
    }

    let output = child
        .wait_with_output()
        .context("等待 PowerShell 结束失败")?;
    let stdout = strip_clixml(&String::from_utf8_lossy(&output.stdout));
    let stderr = strip_clixml(&String::from_utf8_lossy(&output.stderr));

    if !output.status.success() {
        let mut msg = format!(
            "PowerShell 写入失败 (退出码 {})。目标: {}",
            output.status.code().unwrap_or(-1),
            source_file.display()
        );
        if !stderr.is_empty() {
            msg.push('\n');
            msg.push_str(&stderr);
        } else if !stdout.is_empty() {
            msg.push('\n');
            msg.push_str(&stdout);
        }
        bail!(msg);
    }

    if verbose && !stdout.is_empty() {
        for line in stdout.lines() {
            println!("    {line}");
        }
    }
    Ok(())
}

/// Windows PowerShell 在 stdout/stderr 被重定向时会夹带 CLIXML 进度记录。
fn strip_clixml(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    loop {
        match rest.find("#< CLIXML") {
            None => {
                out.push_str(rest);
                break;
            }
            Some(start) => {
                out.push_str(&rest[..start]);
                let after = &rest[start..];
                match after.find("</Objs>") {
                    Some(end) => rest = &after[end + "</Objs>".len()..],
                    None => break,
                }
            }
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_command_is_utf16le_base64() {
        let script = "Write-Host 'hi'";
        let encoded = encode_command(script);
        let bytes = STANDARD.decode(&encoded).unwrap();
        assert_eq!(bytes.len() % 2, 0);
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let decoded: String = String::from_utf16(&units).unwrap();
        assert_eq!(decoded, script);
    }

    #[test]
    fn strip_clixml_keeps_plain_text() {
        let raw = "hello\n#< CLIXML\n<Objs></Objs>\nworld";
        assert_eq!(strip_clixml(raw), "hello\n\nworld");
    }
}
