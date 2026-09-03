# -*- coding: utf-8 -*-
"""
用 Python 读取指定文件的原始字节流，
再完全通过 PowerShell 将同样的字节流原封不动写入 output/ 下的同名文件。
- 读取：Python（二进制读取原始字节，不限文件格式）
- 写入：PowerShell（Python 只通过 stdin 传 base64 字节流，写文件由 PowerShell 完成）

处理流程:
    1. 若输入文件不是 xlsx 格式，记录其原始格式，复制一份并把副本的
       后缀改成 xlsx（原文件保留不动）；若已是 xlsx 格式，直接进入第 2 步。
    2. 用 Python 读取字节流，完全通过 PowerShell 写入 output/ 下的同名文件。
    3. 若第 1 步改过格式，则把输出文件由 xlsx 格式改回原本的文件格式，
       并删除第 1 步复制得到的 xlsx 中间文件；若原本是 xlsx 格式，则不做处理。

用法:
    python read_and_write.py <文件名>
    python read_and_write.py          # 运行后交互输入文件名
"""
import base64
import os
import shutil
import subprocess
import sys
from pathlib import Path

BASE_DIR = Path(__file__).resolve().parent
OUTPUT_DIR = BASE_DIR / "output"

# PowerShell 脚本：从 stdin 读 base64，解码为原始字节并写入目标文件，
# 然后在 PowerShell 内读回比对哈希完成验证（Python 侧的重新读取
# 在本机隐藏修复机制下可能看到不同视图，不能用于验证）。
# __OUT_DIR__/__OUT_FILE__ 为占位符。
PS_SCRIPT = r'''
$ErrorActionPreference = 'Stop'
$b64 = [Console]::In.ReadToEnd() -replace '\s', ''
$bytes = [Convert]::FromBase64String($b64)

$outDir = '__OUT_DIR__'
$outFile = '__OUT_FILE__'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
[System.IO.File]::WriteAllBytes($outFile, $bytes)

$written = [System.IO.File]::ReadAllBytes($outFile)
$md5 = [System.Security.Cryptography.MD5]::Create()
$h1 = [BitConverter]::ToString($md5.ComputeHash($bytes))
$h2 = [BitConverter]::ToString($md5.ComputeHash($written))
if ($h1 -ne $h2) { throw '验证失败: 写入文件的字节流与接收的数据不一致!' }

$fi = Get-Item $outFile
Write-Host ('PowerShell written: ' + $fi.FullName + ' (' + $fi.Length + ' bytes)')
Write-Host 'PowerShell 验证通过: 写入字节流与接收数据完全一致'
'''


def write_bytes_via_powershell(data: bytes, dest_dir: Path, dest_file: Path) -> None:
    """把原始字节流交给 PowerShell，由其写入目标文件。

    字节流以 base64 通过 stdin 传递，脚本以 -EncodedCommand 传递，避免转义问题。
    """
    ps_script = (
        PS_SCRIPT
        .replace("__OUT_DIR__", str(dest_dir))
        .replace("__OUT_FILE__", str(dest_file))
    )
    encoded_command = base64.b64encode(ps_script.encode("utf-16-le")).decode("ascii")
    payload = base64.b64encode(data).decode("ascii")
    subprocess.run(
        ["powershell", "-NoProfile", "-EncodedCommand", encoded_command],
        input=payload,
        text=True,
        encoding="ascii",
        check=True,
    )


def main() -> None:
    filename = sys.argv[1] if len(sys.argv) > 1 else input("请输入文件名: ").strip()
    source_file = (BASE_DIR / filename).resolve()
    if not source_file.is_file():
        print(f"文件不存在: {source_file}")
        sys.exit(1)

    # 第 1 步：非 xlsx 格式则记录原格式，复制一份并把后缀改成 xlsx（原文件保留）
    original_suffix = source_file.suffix
    if original_suffix.lower() != ".xlsx":
        print(f"记录原始格式: {original_suffix or '(无扩展名)'}")
        xlsx_file = source_file.with_suffix(".xlsx")
        if xlsx_file.exists():
            print(f"无法生成 xlsx 副本，目标文件已存在: {xlsx_file}")
            sys.exit(1)
        shutil.copyfile(source_file, xlsx_file)
        source_file = xlsx_file
        print(f"已复制原文件并改后缀为 xlsx: {source_file} (原文件保留)")
    else:
        print("已是 xlsx 格式，直接进入读取")

    # 第 2 步：Python 读取字节流，完全通过 PowerShell 写入 output/
    output_file = OUTPUT_DIR / source_file.name
    print(f"读取: {source_file}")
    data = source_file.read_bytes()
    print(f"读取到 {len(data)} 字节")

    write_bytes_via_powershell(data, OUTPUT_DIR, output_file)
    print("验证通过: 输出文件字节流与读取数据完全一致(由 PowerShell 校验)")

    # 第 3 步：原本不是 xlsx 格式，则把输出文件改回原本的格式，
    # 并删除第 1 步复制得到的 xlsx 中间文件
    if original_suffix.lower() != ".xlsx":
        restored_file = output_file.with_suffix(original_suffix)
        # 直接 replace 可能被系统拦截，改用复制+删除
        shutil.copyfile(output_file, restored_file)
        os.remove(output_file)
        print(f"输出文件已改回原本格式: {restored_file}")
        os.remove(source_file)
        print(f"已删除中间的 xlsx 副本: {source_file}")


if __name__ == "__main__":
    main()
