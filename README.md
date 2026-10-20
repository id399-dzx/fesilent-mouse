# Fesilent Mouse

在同一局域网中共享 Windows / Mac 的键盘鼠标、剪贴板和文件。Mac 默认接收控制，也可在设置中显式开启主控。

基于 [Yujay-Z/MouseShare](https://github.com/Yujay-Z/MouseShare) 的原生 Rust / egui 工具。本 Fork 保留上游核心与作者历史，重新设计设置界面，并改进连接配置体验。来源和权利边界见 [NOTICE.md](NOTICE.md)，上游原说明见 [docs/UPSTREAM-README.md](docs/UPSTREAM-README.md)。

## 0.2.1 的更新

- 使用新的黑白品牌图标，覆盖 Windows EXE、主窗口、Mac Dock 及菜单栏/托盘。
- [下载 v0.2.1 预览版](https://github.com/id399-dzx/fesilent-mouse/releases/tag/v0.2.1)：Windows x64 免安装 EXE 与 Apple Silicon Mac ZIP。

## 0.2.2 的更新

增加单实例工作台：重复打开会恢复已有窗口，多台电脑在“我的设备”中统一添加、选择和管理。工作台自动显示本机的 `IP:端口` 并可一键复制；手动输入对端IP无需查询端口，省略时默认补为 `53728`，显式有效端口保持不变。

[下载 v0.2.2 预览版](https://github.com/id399-dzx/fesilent-mouse/releases/tag/v0.2.2)：Windows x64 免安装 EXE 与 Apple Silicon Mac ZIP，附 SHA256SUMS。

## 0.2.0 的改进

- 官网同风格的原生设备工作台：雾白与淡紫粉背景、白色圆角面板、黑色胶囊按钮，窄窗口自动重排导航。
- 控制端可拖动对端屏幕向上下左右吸附，设备各自保留名称、方位与沿边偏移。
- 可设置快捷键与临时锁定；Mac 主控显式开启，旧配置保持默认接收模式。
- 会话接通权限、心跳与显示变化检查；文件并发同名避让、取消与完整性校验。
- 区分搜索、等待接入、配对、连接与重连，显示引擎当前控制目标；首次图形界面配对需核对六位码，可从设备管理重新配对。
- 手动输入 Mac 的 IPv4 地址时，自动补齐默认端口 `53728`。
- 地址先校验再应用；运行中应用新地址会重启连接，不再显示“即时生效”却继续使用旧地址。
- 连接错误显示在界面中，并说明 Mac 端需要留空地址、启用互控。
- 底部轻量传输入口、真实失败原因、接收目录快捷打开；保留传输速率、取消、大文件确认、剪贴板历史与诊断导出。
- 轻触、标准、防误触三种切换手感预设，保留高级数值调整及恢复默认。
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

首次连接时，两端图形界面会显示六位配对码，核对一致后分别确认。需要重新配对时，请在**两台电脑上都先「忘记已配对设备」**，再重新连接；只忘记一端会导致另一端沿用原信任记录、不再显示配对码。

文本、图片和 HTML 可跨机复制粘贴。复制文件后切到另一台电脑粘贴，接收目录默认是 `下载/FesilentMouse`。Mac 默认接收 Windows 控制；在「使用偏好」开启「允许这台 Mac 主动控制对端」后，也可用 Mac 键鼠控制 Windows。默认 Ctrl+Alt+空格切换/返回，Ctrl+Alt+L 临时锁在本机，组合键可修改。

「我的设备」可保存多台电脑并设置名称、方位和沿边偏移，每次选择一台连接。有其他旧配对的两端需要分别点击「添加新设备」才能接纳新身份；清单记录不等于在线或已授权。

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

仅支持局域网，手动连接目前使用 IPv4。网络继续沿用上游 QUIC/TLS、UDP 53728 和协议，不支持公网远程桌面或屏幕画面传输。本批不提供系统服务，暂不支持 Windows UAC 安全桌面、登录界面和 Mac 锁屏控制；Linux 按用户选择暂缓。

自动构建与界面检查不能替代双机实际互控、Mac 授权和输入手感测试；验证记录会明确区分这些结果。
