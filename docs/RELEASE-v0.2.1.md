Fesilent Mouse v0.2.1 更新为用户设计的黑白品牌图标，覆盖 Windows 主程序、窗口与托盘，以及 Mac 应用、Dock 和菜单栏。

- Windows x64：下载 `fesilent-mouse.exe`，免安装运行。
- Apple Silicon Mac：下载 `fesilent-mouse-macOS-arm64.zip`，解压后运行 `.app`。
- `SHA256SUMS` 提供两个下载包的完整性校验。

新图标保留原始设计与长宽比，Windows 含 16–256px 七档资源，Mac 菜单栏使用透明白色标志模板。继续提供 v0.2.0 的键鼠跨屏、双向剪贴板、复制粘贴文件传输、已保存设备与单活动连接功能，网络协议和用户数据目录保持一致。

发布前已执行格式、严格 Clippy、工作区测试、Mac ARM64/Windows x64 GNU release 构建、图标资源和包结构/SHA 检查。双机实际互控、Windows 桌面图标缓存仍需真机确认。

此版本仍为预览版：Windows EXE 未做 Authenticode 签名；Mac 为 ad-hoc 签名，尚未 Apple 公证。Mac 首次运行需确认打开，并授予辅助功能权限。

旧版 v0.2.0 保留，可从 Releases 页面继续下载。
