# Fesilent Mouse 工程契约（SPEC）

> **Fork 修订（2026-10-19，用户要求更名与 UI 优化）**：下文保留上游历史契约便于溯源。当前显示名为 `Fesilent Mouse`，可执行文件名为 `fesilent-mouse`，版本 `0.2.0`，macOS bundle id 为 `com.fesilent.mouse`。配置、设备身份、日志、接收文件与开机自启标识使用 `FesilentMouse`，不覆盖上游目录。Windows 默认用户安装位置为 `%LOCALAPPDATA%\Programs\FesilentMouse\`，也可直接运行免安装 EXE。crate 名、协议版本、UDP 53728、mDNS 类型和消息格式沿用上游。手动连接只接受局域网 IPv4 或 `IPv4:端口`，裸 IPv4 自动补 53728；非法地址在启动前拒绝。当前目录与 UI 设计详见 `docs/FESILENT-UI.md`。

> 本文件固化所有跨会话必须一致的命名、常量与接口契约。
>
> **Fork 交互与可靠性修订（2026-10-19，用户授权按 Deskflow 对比建议1–4推进）**：Linux按用户选择暂缓。配置仍为 `config_ver:1`，新增字段均有serde默认：`mac_control`默认false保留旧Mac从属行为；`shortcuts_enabled`、`switch_shortcut`、`lock_shortcut`；`onboarding_done`；`display_offset`默认0保留比例映射、非零为沿边物理像素；`devices`保存完整SHA256身份、名称、可选监听地址、四向位置和活动目标。记录不等同信任，trust.json仍独立pinning；单次一个活动会话，切换必须结束旧会话。无新依赖、协议版本/编号/端口不变。当前运行中的键鼠、控制、心跳共用持久可靠QUIC流，Datagram API尚未用于实际输入，旧通道表为设计目标。临时文件只自动清理带内部前缀和UUID的过期暂存，不再按扩展名删除任意文件。
> **修改本文件中任何条目须经人确认**（见 CLAUDE.md §2），修改后需全仓搜索同步引用处。

## 1. 命名与标识

| 项 | 值 |
|---|---|
| 产品名 | MouseShare |
| 可执行文件名 | `mouseshare`（macOS .app 内同名） |
| macOS bundle id | `com.mouseshare.app`（日常/发布版）；测试版用 `com.mouseshare.dev`、显示名 `MouseShare Dev`，与日常版身份隔离避免抢辅助功能授权（脚本 build-macos-app.sh 产出测试版，2026-06-30 起） |
| Windows 安装目录 | `%ProgramFiles%\MouseShare\` |
| Cargo workspace 根包名 | 无根包，纯 virtual workspace |
| crate 命名 | `ms-core` `ms-proto` `ms-net` `ms-platform` `ms-platform-windows` `ms-platform-macos` `ms-app`（目录名见 DEVELOPMENT.md §6.1，目录用短名 core/proto/…，包名带 `ms-` 前缀防 crates.io 冲突） |
| 设备 ID | UUID v4，首次启动生成并持久化 |

## 2. 网络常量

| 项 | 值 | 说明 |
|---|---|---|
| QUIC 监听端口（默认） | **UDP 53728** | 可在配置中改；避开常用端口与 Synergy 的 24800 |
| mDNS 服务类型 | `_mouseshare._udp.local.` | |
| mDNS TXT 字段 | `ver`（协议版本）、`id`（device_id）、`name`（设备名） | |
| 协议版本 `proto_ver` | 从 `1` 起，u16 | 不兼容变更 +1 |
| 心跳间隔 / 判死 | 1s / 连续 3 次无 Pong（3s） | FR-601 |
| QUIC idle timeout | 8s | DEVELOPMENT.md §4.4 |
| QUIC DSCP 标记 | **EF（DSCP 46 / IP_TOS 0xB8）** | NFR-08 QoS 优先级；macOS/Linux 经 `socket2::set_tos`，Windows 经 qWAVE `QOSTrafficTypeVoice`（自动 EF）。常量 `ms-net::qos::DSCP_EF` |
| 重连退避 | 0.5, 1, 2, 4, 8, 10, 10, …（秒，封顶 10） | FR-602 |
| 剪贴板单次上限 | 10 MiB（压缩前） | FR-403 |
| 剪贴板分块大小 | 256 KiB | |
| 文件传输分块大小 | 1 MiB | |

## 3. 消息类型编号（`[u8 类型][u32 长度 LE][bincode 载荷]`，载荷上限 2 MiB）

> 修订（2026-06-13，待人追认）：长度字段原为 u16，与 §2 的 256KiB/1MiB 分块上限矛盾（u16 最大 64KiB），实现 T-101 时改为 u32 并增加 2 MiB 载荷上限。

编号一经发布不得复用；废弃的编号永久保留注释占位。

| 编号 | 消息 | 通道 |
|---|---|---|
| 0x01 | Hello | Stream 0 |
| 0x02 | PairRequest | Stream 0 |
| 0x03 | PairConfirm | Stream 0 |
| 0x04 | ScreenInfo | Stream 0 |
| 0x05 | LayoutUpdate | Stream 0 |
| 0x06 | EnterScreen | Stream 0 |
| 0x07 | LeaveScreen | Stream 0 |
| 0x08 | Ping | Stream 0 |
| 0x09 | Pong | Stream 0 |
| 0x0A | ReleaseAll | Stream 0 |
| 0x0B | Suspend（睡眠预告） | Stream 0 |
| 0x10 | KeyEvent | Stream 1 |
| 0x11 | MouseButton | Stream 1 |
| 0x12 | Scroll | Stream 1 |
| 0x18 | MouseMove | Datagram |
| 0x20 | ClipboardData | Stream 2 |
| 0x30 | FileOffer | Stream 3+ |
| 0x31 | FileAccept | Stream 3+ |
| 0x32 | FileChunk | Stream 3+ |
| 0x33 | FileDone | Stream 3+ |
| 0x34 | FileCancel | Stream 3+ |

字段定义以 `ms-proto` 中的 Rust enum 为准（初稿见 DEVELOPMENT.md §4.2）；本表只锁定编号与通道。

## 4. 文件路径与持久化

| 项 | Windows | macOS |
|---|---|---|
| 配置 | `%APPDATA%\MouseShare\config.json` | `~/Library/Application Support/MouseShare/config.json` |
| 日志 | `%LOCALAPPDATA%\MouseShare\logs\` | `~/Library/Logs/MouseShare/` |
| 私钥/信任列表 | 配置目录文件 `cert.der`/`key.der`/`trust.json` | 同左（`~/Library/Application Support/MouseShare/`） |
| 文件接收目录（默认） | `%USERPROFILE%\Downloads\MouseShare\` | `~/Downloads/MouseShare/` |

配置 schema：`config.json` 顶层带 `"config_ver": 1`；新增字段必须有默认值（serde default），保证旧配置可读。

> 私钥存储变更（2026-06-14，经用户确认）：原计划私钥存 Keychain/DPAPI 加密，但
> 开发期未签名二进制每次读 Keychain 会弹密码授权窗，体验差。改为存配置目录文件
> 并设 `0600` 权限（仅当前用户可读）。安全性略低于钥匙串加密，但对个人 LAN 工具
> 可接受；正式签名发布版可再评估切回 Keychain（届时不弹窗）。

## 5. 断连/回退 reason code（tracing 日志与 UI 共用）

```rust
pub enum DisconnectReason {
    HeartbeatTimeout,   // 心跳 3 次无响应
    QuicIdle,           // QUIC idle timeout
    QuicError,          // 传输层错误
    PeerSuspend,        // 对端睡眠预告
    PeerShutdown,       // 对端正常退出
    LocalShutdown,      // 本端退出
    AuthRejected,       // 指纹不在信任列表
    PermissionRevoked,  // macOS 权限被撤销（TECH-REVIEW A-1）
    HookLost,           // Windows 钩子被摘除且重装失败（A-3）
    TapDisabledLoop,    // macOS tap 频繁被禁用熔断（A-2）
    VersionMismatch,    // 协议版本不兼容
}
```

新增原因码只增不改；UI 文案映射表与该枚举同步维护于 `ms-core`。

## 6. 依赖基线（AI 可自由使用，版本只锁主版本）

tokio 1、quinn 0.11+、rustls 0.23+、bincode 2、serde/serde_json 1、mdns-sd 0.13+、
windows 0.6x（windows-rs）、objc2 0.6+、core-graphics 0.25+、core-foundation 0.10+/core-foundation-sys 0.8+、
tray-icon 0.23+/tao 0.35+/muda 0.19+（系统托盘，替代 tauri——见下），
tracing/tracing-subscriber/tracing-appender 0.x、crossbeam 0.8、uuid 1、thiserror 2、
anyhow 1（仅 app 层）、proptest 1（dev）、zstd 0.13、ed25519-dalek 2、rcgen 0.13、
sha2 0.10、bytes 1（quinn datagram API）、rustls-pki-types 1、futures-util 0.3、crossbeam-queue 0.3、
socket2 0.5（QUIC DSCP socket，与 tokio 同版复用）、
security-framework 3（macOS Keychain）、windows 0.59（DPAPI 等 Win32；qWAVE 需 feature `Win32_NetworkManagement_QoS`/`Win32_Networking_WinSock`）。

- Rust：stable 最新，edition 2024；MSRV 不刻意维护（v1.0 前）。
- 基线之外的依赖：先问人（CLAUDE.md §2）。

> 托盘技术变更（2026-06-14，经用户确认）：原定 tauri 2 做托盘+设置窗口，但 tauri
> 含 WebView2，**无法从 mac 交叉编译到 Windows**（Windows 托盘版需真机编译）。改用
> tauri 底层同款的 tray-icon + tao + muda（纯托盘+事件循环，无 webview），mac/Windows
> **都能 zigbuild**，实现两端对称的任务栏托盘。设置走原生菜单项（不再用 webview 窗口）。
- `Cargo.lock` 提交入库。

## 7. Git 与代码规范

- 分支：直接在 `main` 开发（单 AI 工作流，无 PR 流程）；发布打 tag `v0.x.y`。
- Commit 格式：`<type>(<scope>): <中文摘要>`；type ∈ feat/fix/refactor/test/docs/chore/ci；scope 用 crate 短名（core/proto/net/platform/win/mac/app/ui）。每个 commit 末尾带任务号，如 `(T-103)`。
- 注释与日志文案：中文；标识符与 tracing 字段名：英文。
- 错误处理：库 crate 用 `thiserror` 定义错误，`ms-app` 才允许 `anyhow`；禁止 `unwrap`/`expect` 出现在非测试代码（平台层 FFI 边界确有不变量时允许 `expect` + 注释）。
- `unsafe`：仅 platform-* crate **及 ms-app 的 ui.rs（GUI 的 GL 上下文创建经 glutin 必须 unsafe，集中隔离在 `GlutinWindowContext`）**，必须 `// SAFETY:` 注释；clippy 开 `undocumented_unsafe_blocks` lint。ms-app 因此单独配 lints（unsafe_code=allow + 上述 clippy deny），不继承 workspace 的 unsafe_code=deny（2026-06-16 winit UI 重构引入，CLAUDE.md §5 红线相应放宽）。
- CLI 调试参数（`ms-app`）：`--loopback`、`--port <u16>`、`--record <path>`、`--replay <path>`、`--no-tray`（headless 测试用）、`--no-clipboard`（关闭剪贴板同步**与文件传输**，二者同走"切换时读剪贴板"机制，FR-404）。
- 日志开关：`RUST_LOG=mouseshare=debug`（所有 crate 的 tracing target 统一以 `mouseshare::<crate>` 为前缀）。
- **文件传输（T-504）**：实现位于 `ms-net::transfer`（开源核心），默认构建即可用，无需 feature/license。**注**：付费/分发模型（曾试验的闭源 `premium/ms-premium` + license 门控）已**暂缓**——按用户决定先把软件做到完全可用，发布与商业化模式留待后续。`premium/` 目录保留但当前**不参与**开源构建，`crates/premium-api` 暂为未接线的预留 crate。

## 8. 坐标与单位约定

- 内部坐标系：**左上原点，y 向下，f64 物理像素**；macOS 的 y 翻转只发生在 `ms-platform-macos` 内部（DEVELOPMENT.md §3.2）。
- `ratio`：边缘归一化位置 0.0–1.0，沿该边从左→右 / 上→下方向。
- 键码：内部统一 USB HID Usage ID（u16，Keyboard/Keypad page 0x07）；映射表放 `ms-platform` 共享。
- 时间戳：消息内用 `u64` 毫秒（本端单调时钟），不做跨机时钟同步假设。
