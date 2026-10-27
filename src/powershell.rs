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

/// 单次调用脚本（供单文件或无常驻会话回退使用）。
/// 已移除低效的全文正则 `-replace '\s', ''`，因为 Rust 侧标准 Base64 编码不含空白字符。
const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false)
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$b64 = [Console]::In.ReadToEnd().Trim()
$bytes = [Convert]::FromBase64String($b64)

$outDir = $env:GOODNICE_OUT_DIR
$outFile = $env:GOODNICE_OUT_FILE
if ([string]::IsNullOrWhiteSpace($outDir) -or [string]::IsNullOrWhiteSpace($outFile)) {
    throw '内部错误: 未设置 GOODNICE_OUT_DIR / GOODNICE_OUT_FILE'
}
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
[System.IO.File]::WriteAllBytes($outFile, $bytes)

$written = [System.IO.File]::ReadAllBytes($outFile)
if ($bytes.Length -ne $written.Length) {
    throw '验证失败: 写入文件的长度与接收的数据不一致!'
}
$md5 = [System.Security.Cryptography.MD5]::Create()
$h1 = $md5.ComputeHash($bytes)
$h2 = $md5.ComputeHash($written)
for ($i = 0; $i -lt $h1.Length; $i++) {
    if ($h1[$i] -ne $h2[$i]) {
        throw '验证失败: 写入文件的字节流与接收的数据不一致!'
    }
}

if ($env:GOODNICE_VERBOSE -eq '1') {
    $fi = Get-Item -LiteralPath $outFile
    [Console]::Out.WriteLine('PowerShell written: ' + $fi.FullName + ' (' + $fi.Length + ' bytes)')
    [Console]::Out.WriteLine('PowerShell 验证通过: 写入字节流与接收数据完全一致')
}
"#;

/// 常驻 PowerShell 工作进程脚本：
/// 批量处理多个文件时只启动一次 PowerShell / .NET 运行时，复用同一个 MD5 实例，消除每个文件 300~600ms 的进程启动开销。
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
        $parts = $header.Split('|', 3)
        if ($parts.Length -lt 3) {
            throw '内部协议错误: 请求头格式无效'
        }
        $verbose = $parts[0]
        $outDir = $utf8.GetString([Convert]::FromBase64String($parts[1]))
        $outFile = $utf8.GetString([Convert]::FromBase64String($parts[2]))
        $b64 = [Console]::In.ReadLine()
        if ($null -eq $b64) {
            throw '内部协议错误: 未收到文件字节流负载'
        }
        $bytes = [Convert]::FromBase64String($b64)

        if (-not [System.IO.Directory]::Exists($outDir)) {
            [System.IO.Directory]::CreateDirectory($outDir) | Out-Null
        }
        [System.IO.File]::WriteAllBytes($outFile, $bytes)

        $written = [System.IO.File]::ReadAllBytes($outFile)
        if ($bytes.Length -ne $written.Length) {
            throw '验证失败: 写入文件的长度与接收的数据不一致!'
        }
        $h1 = $md5.ComputeHash($bytes)
        $h2 = $md5.ComputeHash($written)
        for ($i = 0; $i -lt $h1.Length; $i++) {
            if ($h1[$i] -ne $h2[$i]) {
                throw '验证失败: 写入文件的字节流与接收的数据不一致!'
            }
        }

        if ($verbose -eq '1') {
            [Console]::Out.WriteLine('GOODNICE_LOG|PowerShell written: ' + $outFile + ' (' + $written.Length + ' bytes)')
            [Console]::Out.WriteLine('GOODNICE_LOG|PowerShell 验证通过: 写入字节流与接收数据完全一致')
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

    pub fn write_bytes(
        &mut self,
        data: &[u8],
        dest_dir: &Path,
        dest_file: &Path,
        verbose: bool,
    ) -> Result<()> {
        let dir_b64 = STANDARD.encode(dest_dir.to_string_lossy().as_bytes());
        let file_b64 = STANDARD.encode(dest_file.to_string_lossy().as_bytes());
        let v_flag = if verbose { "1" } else { "0" };

        writeln!(self.stdin, "{v_flag}|{dir_b64}|{file_b64}")
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
                    dest_file.display()
                );
            }
            let trimmed = line.trim();
            if let Some(log_msg) = trimmed.strip_prefix("GOODNICE_LOG|") {
                if verbose {
                    println!("    {log_msg}");
                }
            } else if trimmed == "GOODNICE_OK" {
                return Ok(());
            } else if let Some(err_msg) = trimmed.strip_prefix("GOODNICE_ERR|") {
                bail!(
                    "PowerShell 写入失败: {err_msg}。目标: {}",
                    dest_file.display()
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

/// 把原始字节流交给单次 PowerShell 进程，由其写入目标文件并校验 MD5。
pub fn write_bytes_via_powershell(
    powershell: &OsString,
    data: &[u8],
    dest_dir: &Path,
    dest_file: &Path,
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
        .env("GOODNICE_OUT_DIR", dest_dir.as_os_str())
        .env("GOODNICE_OUT_FILE", dest_file.as_os_str())
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
            dest_file.display()
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
