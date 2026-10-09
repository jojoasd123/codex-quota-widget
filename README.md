# Codex Quota Widget · Codex 额度小窗

由 Codex 开发的开源 Windows 原生额度小窗，查看已登录 ChatGPT 账号的 Codex 剩余额度、恢复时间和可用重置次数。

## Agent 安装流程

以下说明面向执行安装、配置和维护任务的 Agent。

1. 确认目标为 Windows。GUI 仅支持 Windows；Linux/macOS 构建提供命令行查询。
2. 检查已有 Codex 桌面应用或 CLI，复用所选环境的 ChatGPT 登录。缺少登录时，让用户完成登录，不读取或输出凭据。
3. 下载最新 release 的 Windows x64 portable 包，解压到持久目录。升级前退出旧小窗并备份现有文件，保留用户的 `codex-quota.json`。
4. 选择连接并运行 `--check` 验证。验证通过后启动 GUI；需要桌面入口时创建快捷方式。
5. 向用户报告安装位置、实际连接来源和验证结果。

已安装并登录 GitHub CLI 的环境可用以下 PowerShell 命令下载：

```powershell
$quotaStage = Join-Path $env:TEMP ('codex-quota-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $quotaStage | Out-Null
gh release download --repo jojoasd123/codex-quota-widget `
  --pattern 'CodexQuota-*-windows-x64.zip' --dir $quotaStage
if ($LASTEXITCODE -ne 0) { throw 'Release download failed' }
$quotaArchive = Get-ChildItem -LiteralPath $quotaStage -Filter '*.zip'
Expand-Archive -LiteralPath $quotaArchive.FullName `
  -DestinationPath (Join-Path $quotaStage 'app')
```

没有 `gh` 时，通过 GitHub Releases API 获取最新 release 的同名资产。安装目录由 Agent 按用户环境决定，不照搬其他机器的盘符或用户名。

## Agent 连接配置

默认无需配置。程序依次尝试 Windows PATH 中的 `codex.exe`、当前用户 Codex 桌面应用自带 CLI、常规 npm 全局安装、WSL 默认发行版及其他已注册发行版。

WSL 使用各发行版默认用户，通过登录 shell 的 PATH 和当前用户的 `.local/bin`、`.npm-global/bin`、`.cargo/bin` 寻找 Codex。没有固定发行版名、用户名或 home 路径。发行版与用户名以独立进程参数传递。

自动模式在启动或读取失败时继续尝试其他连接。多个环境登录不同账号时，Agent 应按用户指定的账号环境固定连接，不能仅凭首次成功结果判断账号正确。

在 exe 同目录写入 UTF-8 无 BOM 的 `codex-quota.json`；参考 `codex-quota.example.json`。

| 字段 | 用途 |
| --- | --- |
| `backend` | `auto`（默认）、`native` 或 `wsl` |
| `codexPath` | 本机 CLI 路径；Windows 使用实际 `codex.exe`，不是 `.cmd` 包装器 |
| `wslDistro` | 指定 WSL 发行版；省略时使用默认值 |
| `wslUser` | 指定 Linux 用户；省略时使用该发行版默认用户 |

固定 Windows 连接：

```json
{"backend":"native"}
```

指定 CLI；Agent 应替换为实际检测到的路径：

```json
{"backend":"native","codexPath":"D:\\tools\\codex.exe"}
```

固定 WSL；Agent 应替换为实际发行版和用户：

```json
{"backend":"wsl","wslDistro":"Debian","wslUser":"your-linux-user"}
```

也可用 `CODEX_QUOTA_CONFIG` 指向配置文件，或 `CODEX_QUOTA_CODEX` 指向本机 CLI。JSON 中的 `codexPath` 优先于后者。非标准 npm 安装位置应显式指定实际二进制。

## Agent 验证与启动

将 `$quotaInstallDir` 设置为完成安装的目录，再执行：

```powershell
$quotaExe = Join-Path $quotaInstallDir 'CodexQuota.exe'
$quotaCheck = Join-Path $env:TEMP ('codex-quota-check-' + [guid]::NewGuid() + '.json')
$quotaProcess = Start-Process -FilePath $quotaExe -WindowStyle Hidden -PassThru `
  -ArgumentList @('--check', ('"{0}"' -f $quotaCheck))
if (-not $quotaProcess.WaitForExit(60000)) {
  throw 'Quota check has not completed; inspect the connection before continuing'
}
$quotaProcess.Refresh()
if ($quotaProcess.ExitCode -ne 0) { throw 'Quota check failed; inspect the check JSON' }
$quotaResult = Get-Content -LiteralPath $quotaCheck -Raw | ConvertFrom-Json
if (-not $quotaResult.ok -or @($quotaResult.windows).Count -eq 0) {
  throw 'No quota windows returned'
}
$quotaResult | Select-Object source, windows
Start-Process -FilePath $quotaExe -WorkingDirectory $quotaInstallDir
```

`--check <输出文件>` 查询后退出：成功 exit 0，失败 exit 1。JSON 包含 `ok`、`source`、`windows`，失败时包含 `error`。确认实际来源符合用户指定的环境，且 `remainingPercent` 在 0–100 范围内。诊断文件包含个人额度，不应提交到仓库或上传为资产。

创建桌面快捷方式时，`TargetPath` 指向安装后的 exe，`WorkingDirectory` 指向安装目录，`IconLocation` 使用同目录 `CodexQuota.ico,0`。不要依赖下载临时目录。

GUI 每 60 秒刷新，支持 F5 / 手动刷新、置顶、托盘和额外额度展开。Acrylic 不可用、关闭透明效果或启用高对比度时，窗口回退到实色背景。

## Agent 故障处理

- 未找到 Codex：检查实际二进制并配置 `codexPath`，无需迁移凭据。
- 未登录或只有 API key：让用户在选定环境完成 ChatGPT 登录，再运行诊断。
- WSL 失败：用 `wsl --list --quiet` 确认发行版，检查默认用户及 Codex 的 PATH；必要时固定连接参数。
- 多账号或错误来源：显式指定连接，重新验证 `source`。
- 网络失败：保留配置和登录，修复连接后重试，不将失败当成零额度。
- 服务端字段变化：参照 [Codex App Server](https://learn.chatgpt.com/docs/app-server#6-rate-limits-chatgpt) 更新解析和契约测试。

每次刷新启动短时 Codex `app-server`，通过 stdio 调用 `account/rateLimits/read`。不执行模型推理，不使用重置机会，不直接打开、复制或上传登录凭据。缺失字段显示未知，网络失败保留最近结果。重置详情可能缺失或截断，以服务端数量为准。

## Agent 开发与发布

使用 Rust stable。Windows 需要 MSVC 工具链和 Visual Studio C++ Build Tools：

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo build --locked --release
Copy-Item target/release/codex-quota-widget.exe CodexQuota.exe
```

Linux 交叉编译 Windows 需要 MinGW：

```sh
rustup target add x86_64-pc-windows-gnu
cargo test --locked
cargo build --locked --release --target x86_64-pc-windows-gnu \
  --config 'target.x86_64-pc-windows-gnu.linker="x86_64-w64-mingw32-gcc"'
```

GitHub Actions 验证格式、测试和 Windows Clippy，并构建 portable artifact。实时查询在用户本机验证，不向 CI 提供账号凭据。发布包仅包含 exe、图标、README、许可证和示例配置，排除个人配置、凭据、诊断结果和截图。

## License

MIT。
