use std::ffi::OsString;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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

/// 从 stdin 读 base64，解码为原始字节并写入目标文件，再在 PowerShell 内比对 MD5。
/// 路径通过环境变量传入，避免拼接进脚本时的转义问题。
const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$b64 = [Console]::In.ReadToEnd() -replace '\s', ''
$bytes = [Convert]::FromBase64String($b64)

$outDir = $env:GOODNICE_OUT_DIR
$outFile = $env:GOODNICE_OUT_FILE
if ([string]::IsNullOrWhiteSpace($outDir) -or [string]::IsNullOrWhiteSpace($outFile)) {
    throw '内部错误: 未设置 GOODNICE_OUT_DIR / GOODNICE_OUT_FILE'
}
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
[System.IO.File]::WriteAllBytes($outFile, $bytes)

$written = [System.IO.File]::ReadAllBytes($outFile)
$md5 = [System.Security.Cryptography.MD5]::Create()
$h1 = [BitConverter]::ToString($md5.ComputeHash($bytes))
$h2 = [BitConverter]::ToString($md5.ComputeHash($written))
if ($h1 -ne $h2) { throw '验证失败: 写入文件的字节流与接收的数据不一致!' }

if ($env:GOODNICE_VERBOSE -eq '1') {
    $fi = Get-Item $outFile
    [Console]::Out.WriteLine('PowerShell written: ' + $fi.FullName + ' (' + $fi.Length + ' bytes)')
    [Console]::Out.WriteLine('PowerShell 验证通过: 写入字节流与接收数据完全一致')
}
"#;

const CANDIDATES: &[&str] = &["powershell.exe", "pwsh.exe", "powershell", "pwsh"];

/// 查找本机可用的 PowerShell。优先 Windows PowerShell 5.x（与原 Python 脚本一致）。
pub fn detect_powershell() -> Result<OsString> {
    let mut last_err = None;
    for prog in CANDIDATES {
        match powershell_command(prog)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-NoLogo",
                "-Command",
                "$ProgressPreference='SilentlyContinue'; exit 0",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => return Ok(OsString::from(prog)),
            Ok(output) => {
                last_err = Some(anyhow!(
                    "{prog} 退出码 {}",
                    output.status.code().unwrap_or(-1)
                ));
            }
            Err(err) => last_err = Some(err.into()),
        }
    }
    Err(last_err
        .unwrap_or_else(|| anyhow!("未找到 PowerShell"))
        .context("未找到可用的 PowerShell（powershell.exe / pwsh）。写入必须由 PowerShell 完成"))
}

fn encode_command(script: &str) -> String {
    let utf16: Vec<u16> = script.encode_utf16().collect();
    let mut bytes = Vec::with_capacity(utf16.len() * 2);
    for unit in utf16 {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    STANDARD.encode(bytes)
}

/// 把原始字节流交给 PowerShell，由其写入目标文件。
///
/// 字节流以 base64 通过 stdin 传递，脚本以 `-EncodedCommand` 传递，避免转义问题。
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
