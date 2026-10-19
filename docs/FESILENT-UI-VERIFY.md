# Fesilent 设备工作台验证记录

日期：2026-10-19。对应任务：F-1004。视觉参考：https://fesilent.com/data-processing。

## 已完成的自动验证

- `cargo fmt --all --check` 通过。
- `cargo clippy --workspace --all-targets --locked -- -D warnings` 通过。
- `cargo test --workspace --locked`：210 通过、0 失败、9 个原有人工/网络场景测试保持忽略，本轮未新增忽略或降低断言。
- macOS ARM64 release 构建、应用打包、`lipo -verify_arch arm64` 和 ad-hoc `codesign --verify --deep --strict` 通过。
- Windows x64 GNU/Zig release 构建通过，PE32+ GUI 子系统确认，导入表仅含 Windows 系统 DLL，无额外 MinGW 运行库。Zig 链接器有既有的「忽略旧优化设置」提示，不影响产物生成；本机 Clippy 保持零警告门槛。
- TLS 配对测试使用真实 QUIC 回环，验证未确认不写信任、拒绝不保存、批准后保存、停止/超时退出。
- 捕获前握手可取消；停止等待控制流或首条消息均可及时返回，已启动输入捕获后的安全清理保持原逻辑。
- 文件失败原因进入当前视图和历史，迟到的错误不能覆盖完成/取消终态；文件接收回归测试改用显式注入的隔离目录，不修改进程 `HOME`。

编译日志位于项目忽略目录 `logs/ui-*.log`。项目本地工具链位于 `.tools/`，不修改全局 shell 配置。

## 已完成的原生界面检查

使用同一份 Rust / egui 界面，在 macOS 原生窗口内通过 `FESILENT_UI_PREVIEW` 隔离检查；Windows 指该界面的平台分支，不表示已在 Windows 真机启动。预览不连接真实对端、不捕获输入、不改用户配置、启动项或配对记录，连接状态为内存模拟。

- 官网雾白、淡紫粉光晕、黑色胶囊按钮、白色大圆角面板及系统字体正常显示。
- Windows 分支：方向按钮切换左侧后，设备图、箭头、穿越边缘及文字同步更新；将 Mac 拖回右侧后，配置和选中状态同步。
- 使用偏好：选择「防误触」后，展开高级设置显示进入 180 px、返回 120 px，保存反馈正常。
- 窗口缩小至约 752 × 620 时，侧栏切换为顶部导航，内容可滚动、底部入口仍可见。
- Mac 分支：设备排列只读，显示方位在 Windows 上设置；只显示在 Mac 生效的 Ctrl / Command 交换，不显示 Windows 的滚轮倍率和四向编辑。
- 共享记录、使用偏好与工作空间之间可切换。

原生截图保存在 `dist/ui-workspace-windows.png` 和 `dist/ui-workspace-mac.png`。截图中的已连接状态为隔离预览模拟。

## 本地交付

- `dist/fesilent-mouse.app`：Apple Silicon Mac 应用。
- `dist/fesilent-mouse-macOS-arm64.zip`：保留应用签名的压缩包。
- `dist/windows-x64/fesilent-mouse.exe`：Windows x64 便携应用。
- `dist/SHA256SUMS`：两个交付文件的 SHA-256，交付前重新计算并核对。

本轮仅生成本地产物和本地 Git 提交，未推送 GitHub、发布 Release 或替换用户正在使用的 MouseShare。

## 待双机回归（H-2 / H-5）

自动化和预览不能替代下列真机检查：

1. 两端运行新包，Mac 在系统设置中授予该应用辅助功能权限，Windows 连接同一局域网并允许本应用网络访问。
2. 首次连接时，两端分别核对同一个六位码并确认；拒绝后应停止本轮连接，点击重新连接可再次开始。
3. Windows 上切换上下左右四个方位，验证真实鼠标跨屏方向与界面一致；断开时按键均释放。
4. 确认 Ctrl / Command、滚轮速度及方向反转、三种切换手感预设符合预期。
5. 跨机复制文本、图片与文件；验证大文件确认、取消、失败提示和接收后打开目录。
6. 重新配对时，先在两台电脑上都忘记已配对设备，再重新连接并核对配对码。退出/重开后检查设置仍保存、托盘操作正常。
