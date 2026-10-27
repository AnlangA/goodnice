# AGENTS.md

## 核心红线要求（最高优先级）

1. **严禁使用任何 CI/CD（没有钱）**
   - **绝对禁止**在本项目中创建、配置或启用任何形式的 CI/CD 流水线（包括但不限于 `.github/workflows/` GitHub Actions、GitLab CI、AppVeyor、CircleCI 等）。
   - 原因：没有预算，不消耗任何云端构建额度或付费 Runner 资源。
   - 所有的编译、测试、打包与版本发布（GitHub Release）**必须全部在本地执行完成**，直接上传本地构建产物（如 `dist/setup.exe`）。

2. **严禁直接将 `python.exe` 暴露到 `PATH` 或右键命令中**
   - 本工具编译后的核心可执行文件名为 `python.exe`（位于安装目录 `C:\Program Files\Dot\python.exe`），但绝不能把安装目录直接加入 `PATH`，以免覆盖或干扰用户系统的 Python 环境。
   - **开始菜单与命令行**：安装时在系统开始菜单目录 `C:\ProgramData\Microsoft\Windows\Start Menu\Programs\Dot\` 下创建 `dot.lnk` 快捷方式（及隐藏的同目录转发脚本 `dot.cmd`），并将该开始菜单目录注入系统 `PATH` 环境变量。用户在终端输入 `dot` 即可调用本工具。
   - **右键菜单**：右键菜单名称为 **“自动解密”**（支持文件、文件夹递归 `-r`、文件夹空白处），注册表命令统一通过开始菜单中的 `dot.lnk` 调用。

---

## 本地开发、打包与发布流程

### 1. 本地测试与检查
```powershell
cargo test
cargo clippy --all-targets -- -D warnings
```

### 2. 本地构建 Windows 安装包
```powershell
.\package.ps1
# 或
cargo run --release --bin package
```
构建完成后会在本地生成单文件独立安装包：`dist\setup.exe`（内嵌 `python.exe` 与 `uninstall.exe`）。

### 3. 本地发布 GitHub Release（无 CI/CD）
先在本地运行 `.\package.ps1` 生成 `dist\setup.exe`，再通过 GitHub CLI 直接上传本地产物：
```powershell
gh release create v0.1.0 .\dist\setup.exe --title "v0.1.0" --notes "Windows 系统级安装包（含开始菜单 dot 快捷方式、PATH 环境变量及右键“自动解密”）"
```
