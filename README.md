# Codex Quota Widget · Codex 额度小窗

个人制作的开源 Windows 原生小窗，用来查看已登录 ChatGPT 账号的 Codex 剩余额度和恢复时间。非 OpenAI 官方产品。

## 使用

1. 安装 Codex 桌面应用或 Codex CLI，并登录 ChatGPT 账号。CLI 用户运行 `codex login`。
2. 下载 Windows portable release，解压后双击 `CodexQuota.exe`。
3. 小窗每 60 秒读取一次额度，F5 或“刷新”立即读取；支持置顶、托盘和额外模型额度展开。

窗口使用 Windows Acrylic 背景、圆角和系统动画设置。关闭透明效果、开启高对比度，或系统不支持对应接口时，会回退到实色背景。GUI 只支持 Windows；Linux/macOS 构建提供命令行查询，尚无对应图形界面。

## 自动发现连接

无需固定磁盘、Windows 用户名、Linux 用户名或 WSL 发行版名称。

默认按顺序尝试能读取额度的连接：

1. Windows PATH 中的 `codex.exe`。
2. 当前用户 Codex 桌面应用自带的 CLI。
3. 常规 npm 全局安装中的 Codex Windows 二进制。
4. WSL 默认发行版，使用它的默认用户。
5. 其他已注册的 WSL 发行版，分别使用默认用户。

WSL 内通过登录 shell 的 PATH，以及当前用户的 `.local/bin`、`.npm-global/bin`、`.cargo/bin` 找到 Codex。没有写死某个 `/home/用户名`。发行版名和用户名直接作为进程参数传递，不拼接为 shell 命令。

Windows 本机优先；某个连接无法启动、未登录或读取失败，会继续尝试其他连接。窗口底部显示成功连接的来源。如果多个环境登录了不同账号，请显式选择连接，避免自动回退到其他账号。

## 可选配置

默认不需要配置。将 `codex-quota.example.json` 复制为与 exe 同目录的 `codex-quota.json`，再按需要修改。文件采用 UTF-8（无 BOM）。

固定使用 Windows 登录：

```json
{"backend":"native"}
```

指定自定义 CLI 路径（Windows 使用实际 `codex.exe`，不是 `.cmd` 包装器）：

```json
{"backend":"native","codexPath":"D:\\tools\\codex.exe"}
```

固定使用一个 WSL 环境：

```json
{"backend":"wsl","wslDistro":"Debian","wslUser":"your-linux-user"}
```

`backend` 支持 `auto`、`native`、`wsl`。省略 `wslDistro` 或 `wslUser` 时使用 WSL 默认值。也可用 `CODEX_QUOTA_CONFIG` 指定配置文件位置，用 `CODEX_QUOTA_CODEX` 指定本机 CLI；JSON 中的 `codexPath` 优先于该环境变量。非标准 npm 安装位置可以用 `codexPath` 指定。

无法读取时，确认选择的环境已经用 ChatGPT 登录，而非仅设置了 API key。安装或登录改变后按 F5 即可重新发现。

## 数据与隐私

每次刷新启动一个短时 Codex `app-server`，通过 stdio 调用 `account/rateLimits/read`，然后关闭进程。工具不执行模型推理，不使用额度重置机会，不直接打开、复制或上传登录凭据。Codex 自身使用所选环境的账号配置连接服务端。

窗口展示剩余比例、恢复时间，以及服务端提供的可用重置次数和详情。缺失字段显示未知；网络失败时保留最近一次数据并提示错误。重置详情可能缺失或截断，以服务端返回的数量为准。

可选诊断命令 `CodexQuota.exe --check check.json` 将查询结果写入指定文件，并退出（成功 exit 0，失败 exit 1）。它包含额度与连接来源，不包含凭据。请不要把个人配置、诊断结果或截图提交到公开仓库。

接口说明：[OpenAI Codex App Server](https://learn.chatgpt.com/docs/app-server#6-rate-limits-chatgpt)。接口或安装目录未来变化时可能需要更新本工具。

## 构建

需要 Rust stable。Windows 使用 Rust MSVC 工具链和 Visual Studio C++ Build Tools：

```powershell
cargo test --locked
cargo clippy --locked --target x86_64-pc-windows-msvc -- -D warnings
cargo build --locked --release
Copy-Item target/release/codex-quota-widget.exe CodexQuota.exe
```

Linux 交叉编译 Windows（需 MinGW 和 Windows GNU target）：

```sh
rustup target add x86_64-pc-windows-gnu
cargo test --locked
cargo build --locked --release --target x86_64-pc-windows-gnu \
  --config 'target.x86_64-pc-windows-gnu.linker="x86_64-w64-mingw32-gcc"'
```

GitHub Actions 检查解析/连接测试、格式、Windows Clippy，并构建 Windows portable artifact。实时账号查询需在用户本机执行，CI 不包含账号凭据。

## License

MIT。欢迎修改和分发；项目不隶属 OpenAI。
