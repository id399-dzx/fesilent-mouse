# Fesilent Mouse

在同一局域网中，用 Windows 的一套键盘鼠标控制 Mac，并共享剪贴板和文件。

基于 [Yujay-Z/MouseShare](https://github.com/Yujay-Z/MouseShare) 的原生 Rust / egui 工具。本 Fork 保留上游核心与作者历史，重新设计设置界面，并改进连接配置体验。来源和权利边界见 [NOTICE.md](NOTICE.md)，上游原说明见 [docs/UPSTREAM-README.md](docs/UPSTREAM-README.md)。

## 0.2.0 的改进

- 浅色原生设置窗口：清晰的连接状态、设备方位、分页设置及可滚动内容。
- 手动输入 Mac 的 IPv4 地址时，自动补齐默认端口 `53728`。
- 地址先校验再应用；运行中应用新地址会重启连接，不再显示“即时生效”却继续使用旧地址。
- 连接错误显示在界面中，并说明 Mac 端需要留空地址、启用互控。
- 保留文件传输进度、速率、取消、大文件确认、剪贴板历史和诊断导出。
- 独立的应用标识、配置、身份与接收目录，避免覆盖原 MouseShare。
- GitHub Actions 自动构建 Windows x64 和 Apple Silicon macOS，执行格式、Clippy 与工作区测试。

## 下载与运行

构建工件位于 [Actions](https://github.com/id399-dzx/fesilent-mouse/actions/workflows/build.yml) 的成功运行页面中。选择与设备对应的 artifact，解压后运行；GitHub 下载 Actions 工件需要登录。正式发布包若有，见 [Releases](https://github.com/id399-dzx/fesilent-mouse/releases)。

- Windows x64：`fesilent-mouse-Windows-x64` 内的 `fesilent-mouse.exe`，免安装运行。
- Apple Silicon macOS：`fesilent-mouse-macOS-arm64` 内的 zip，解压得到 `fesilent-mouse.app`。
- 每个包附有 `SHA256SUMS`。当前包未购买 Windows 签名或 Apple 公证；首次运行可能需要用户手动确认。

## 首次连接

1. 两台电脑连接同一个局域网，并启动软件。
2. **Mac 端选择自动发现，地址留空，然后启用互控。**在系统设置的“隐私与安全性 → 辅助功能”中手动授予本应用权限。
3. Windows 端选择自动发现；若无法发现，改为手动连接并填写 **Mac 的局域网 IPv4**，例如 `192.168.1.20` 或 `192.168.1.20:53728`，再应用并启用互控。不要两端都填写手动地址。
4. 选择 Mac 相对 Windows 屏幕的位置，然后向对应的屏幕边缘继续推鼠标，达到推挤距离后切换控制。

文本、图片和 HTML 可跨机复制粘贴。复制文件后切到另一台电脑粘贴，接收目录默认是 `下载/FesilentMouse`。本版本的 Mac 固定为被控端，控制方向是 Windows → Mac。

## 设置与数据

| 内容 | Windows | macOS |
| --- | --- | --- |
| 配置与设备身份 | `%APPDATA%\FesilentMouse\` | `~/Library/Application Support/FesilentMouse/` |
| 日志 | `%LOCALAPPDATA%\FesilentMouse\logs\` | `~/Library/Logs/FesilentMouse/` |
| 接收文件 | `~/Downloads/FesilentMouse/` | `~/Downloads/FesilentMouse/` |

首次运行不会自动复制原 MouseShare 的设置或私钥。原版和本版不要同时启用互控，以免竞争输入钩子或监听端口。关闭设置窗口会继续在托盘运行；从托盘菜单退出可结束程序。

## 本地构建

需要 Rust stable（edition 2024）及对应平台的 C 工具链。

```powershell
# Windows：在 Visual Studio 开发者 PowerShell 中执行（需要 C++ Build Tools / Windows SDK）
.\scripts\build-windows.ps1
```

```bash
# Apple Silicon macOS
bash scripts/build-macos-app.sh

# 校验
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

开发时使用 `--log-dir ./logs` 将日志保留在项目内。设置环境变量 `FESILENT_SHOW_WINDOW=1` 可在本次启动直接显示设置窗口。完整设计与验证范围见 [docs/FESILENT-UI.md](docs/FESILENT-UI.md)。

## 使用范围

仅支持局域网，手动连接目前使用 IPv4。网络继续沿用上游 QUIC/TLS、UDP 53728 和协议，不支持公网远程桌面或屏幕画面传输。Windows UAC 安全桌面、系统登录界面和 Mac 锁屏不接受输入注入。

自动构建与界面检查不能替代双机实际互控、Mac 授权和输入手感测试；验证记录会明确区分这些结果。
