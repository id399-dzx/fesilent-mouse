# 当前 Mac 安装 v0.2.1

日期：2026-10-20；用户明确要求更新本机版本。

- 从 GitHub v0.2.1 重新下载 Apple Silicon Mac ZIP 与 SHA256SUMS，存入 `dist/downloads/v0.2.1/`。
- 下载包 SHA256 为 `5e1cc0cfadeee66e68ca3804b32b630c7613e9a52f9b6ebf427eeb38feb2977b`，与校验文件及已发布记录一致。
- 解压后通过 ARM64、bundle版本0.2.1、ad-hoc签名、结构与安全参数启动检查。
- 安装路径为 `/Applications/Fesilent Mouse.app`；安装后的可执行文件与下载包逐字节一致。
- 新程序已启动，原生窗口可见，标题为 Fesilent Mouse；启动时仅打开设置界面，未启用互控。
- 原运行程序为 `/Applications/MouseShare.app` 0.1.2，已退出；旧应用与原有数据保留，未迁移私钥或覆盖旧配置。
- 原登录启动项 `~/Library/LaunchAgents/com.mouseshare.app.plist` 已停用并移至 `dist/install-backups/2026-10-20/`；同等登录启动入口改指 `/Applications/Fesilent Mouse.app/Contents/MacOS/fesilent-mouse --tray`，新标签为 `com.fesilent.mouse`。

## 待用户完成 H-1

原生界面显示“需要辅助功能权限”。已打开系统设置的“隐私与安全性 → 辅助功能”，确认 **Fesilent Mouse 开关当前为关闭**、旧 MouseShare 权限为开启。没有代用户切换敏感权限。

请用户手动打开 Fesilent Mouse 的开关，再回到软件启用互控。旧 MouseShare 的授权不会自动转给新 bundle。安装、版本和启动验证已完成；键鼠互控需先完成此授权，真实双机体验仍按既有验收范围处理。
