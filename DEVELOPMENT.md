# MouseShare 开发文档

| 项目 | 内容 |
|---|---|
| 文档版本 | v1.1（按 TECH-REVIEW.md 自检结论修订） |
| 日期 | 2026-06-12 |
| 对应需求文档 | REQUIREMENTS.md v1.1 |

---

## 1. 技术选型

### 1.1 结论

**Rust + 各平台原生 API**，GUI 采用 Tauri（设置界面）+ 原生托盘。

| 层 | 选型 | 理由 |
|---|---|---|
| 核心语言 | Rust (stable, edition 2024) | 内存安全（长驻进程不泄漏/不崩溃，直接服务于稳定性目标）；零成本抽象保证低延迟；跨平台编译成熟 |
| 异步运行时 | tokio | 网络 I/O、心跳定时器、重连状态机的事实标准 |
| 传输层 | QUIC（quinn crate）| 内建 TLS 1.3 强制加密；流多路复用——键鼠事件流与文件传输流互不阻塞（解决 TCP 队头阻塞导致的"传文件时鼠标卡顿"）；连接迁移特性天然适配 IP 变更场景 |
| 序列化 | bincode 2（事件消息）+ serde_json（配置） | 事件消息体积小、编解码快；配置可读可调试 |
| 服务发现 | mdns-sd crate（mDNS/DNS-SD） | 双平台纯 Rust 实现，无需依赖系统 Bonjour 服务状态 |
| Windows 原生 | windows-rs（微软官方 crate） | Win32 API 全覆盖、类型安全 |
| macOS 原生 | objc2 / core-graphics / core-foundation crate | CGEventTap、NSPasteboard 等均有成熟绑定 |
| 设置 GUI | Tauri 2 | WebView 实现复杂设置界面成本低；与 Rust 核心同进程通信简单；包体可控（安装包 <10MB，常驻内存 30–50MB） |
| 托盘/菜单栏 | Tauri 2 内建 TrayIcon API | 双平台统一，无需额外依赖（自检修正 C-1） |
| 日志 | tracing + tracing-appender | 结构化日志、滚动文件，便于排查断连问题 |

### 1.2 备选方案对比（记录决策依据）

- **C++ + Qt**（Deskflow 同款）：可直接参考开源实现，但内存安全与现代工具链劣势明显，长期维护成本高。仅作为实现细节参考（Deskflow 是 GPL，注意只参考思路、不复制代码）。
- **Go**：网络层开发快，但输入捕获/注入必须经 CGO 调底层 API，跨 CGO 边界的回调（如 Windows 钩子、CGEventTap 回调）复杂且有性能隐患。
- **TCP+TLS 替代 QUIC**：实现更简单，作为兜底方案保留抽象层接口（`Transport` trait），若 QUIC 在目标网络遇到 UDP 被限速等问题可切换。

---

## 2. 总体架构

### 2.1 角色模型

采用**对等（peer-to-peer）架构 + 动态控制权**，而非 Deskflow 的固定 server/client：

- 两台设备安装同一程序，地位对等，均常驻运行。
- 任一时刻存在一个**控制端**（物理键鼠所在机）与一个**被控端**。
- 鼠标越过边缘 → 控制权移交。键鼠物理接在哪台机器上无所谓，哪台机器的"本地输入"正在被使用，哪台就是控制端。
- 好处：用户无需理解 server/client 概念；任意一台均可发起配置修改。

### 2.2 进程内分层

```
┌─────────────────────────────────────────────┐
│  UI 层      Tauri 设置窗口 / 托盘菜单 / 通知   │
├─────────────────────────────────────────────┤
│  核心层 core（纯 Rust，平台无关）              │
│   · 控制权状态机   · 屏幕布局/坐标映射          │
│   · 会话管理      · 心跳/重连状态机            │
│   · 剪贴板同步逻辑 · 文件传输逻辑               │
├─────────────────────────────────────────────┤
│  协议层 proto    消息定义 / 编解码 / 版本协商    │
├─────────────────────────────────────────────┤
│  传输层 net      QUIC(quinn) / mDNS 发现      │
├─────────────────────────────────────────────┤
│  平台层 platform（trait 抽象 + 双平台实现）     │
│   InputCapture  InputInject  ClipboardAccess │
│   ScreenInfo    Autostart    SecureStore     │
└─────────────────────────────────────────────┘
```

平台层 trait 定义（示意）：

```rust
pub trait InputCapture: Send {
    /// 开始捕获本机键鼠事件；grab=true 时拦截事件不让本机响应（控制对端期间）
    fn start(&mut self, sink: EventSink, grab: bool) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    /// 将光标钉在屏幕中心并隐藏（控制对端期间防止本机光标移动）
    fn confine_cursor(&mut self, on: bool) -> Result<()>;
}

pub trait InputInject: Send {
    fn key(&mut self, code: KeyCode, down: bool) -> Result<()>;
    fn mouse_move_to(&mut self, x: f64, y: f64) -> Result<()>;
    fn mouse_button(&mut self, btn: Button, down: bool) -> Result<()>;
    fn scroll(&mut self, dx: f64, dy: f64) -> Result<()>;
    /// 释放所有按下状态（断连/切换时调用，FR-305）
    fn release_all(&mut self) -> Result<()>;
}
```

### 2.3 控制权状态机

```
            鼠标到达边缘(含延迟/修饰键条件)
 ┌──────────┐ ──────────────────────────▶ ┌──────────┐
 │ LOCAL    │                             │ REMOTE   │
 │ 控制本机  │ ◀────────────────────────── │ 控制对端  │
 └──────────┘   对端光标到达对侧边缘 /        └──────────┘
      ▲          热键 / 连接断开(强制回LOCAL)      │
      └────────────── 断连: release_all ─────────┘
```

**进入/退出 REMOTE 必须是固定顺序的事务**（Deskflow #9779 的光标跳变 bug 即因顺序不全导致，见 TECH-REVIEW B-2），热键切换与边缘切换共用同一段事务代码：

```
进入 REMOTE（本机为控制端）:
  1. warp 本机光标到屏幕中心，并重置位移参考点
  2. 初始化影子光标 = 对端入口坐标
  3. 捕获层 grab=true（事件不再给本机）
  4. 隐藏本机光标
  5. 发送 EnterScreen

退出 REMOTE（任何路径：边缘返回 / 热键 / 断连）:
  1. 解除 grab
  2. warp 本机光标到返回位置（边缘对应点；断连时保持中心）
  3. 显示本机光标
  4. 本端 release_all()；向对端发 ReleaseAll（断连时跳过）
```

---

## 3. 平台层实现要点

### 3.1 Windows

| 能力 | API | 说明 |
|---|---|---|
| 键盘捕获 | `SetWindowsHookExW(WH_KEYBOARD_LL)` | 低级钩子可吞掉事件（返回 1）实现 grab；钩子回调须 <1ms 返回，事件经无锁队列转发 |
| 鼠标捕获 | `SetWindowsHookExW(WH_MOUSE_LL)` + Raw Input(`WM_INPUT`) | 钩子负责拦截，Raw Input 提供未加速的相对位移（控制对端时用相对量更平滑） |
| 注入 | `SendInput` | 键盘用 scancode（`KEYEVENTF_SCANCODE`）而非 VK，兼容性最好；鼠标绝对移动用归一化坐标(0–65535) |
| 光标隐藏/限制 | `ClipCursor` + 每帧重置位置 | 进入 REMOTE 时把光标钉在中心 |
| 剪贴板 | `OpenClipboard`/`GetClipboardData`、`AddClipboardFormatListener` | 格式：`CF_UNICODETEXT`、`CF_HTML`、`CF_DIB`/PNG |
| 屏幕信息 | `EnumDisplayMonitors`、`GetDpiForMonitor` | 进程声明 Per-Monitor V2 DPI Aware |
| 开机自启 | 注册表 `HKCU\...\Run` | |
| 凭据存储 | DPAPI (`CryptProtectData`) | 私钥/信任列表加密落盘 |

**注意事项（按重要性排序）**：

1. **钩子静默移除与看门狗（TECH-REVIEW A-3）**：回调超时（Win10 1709+ 上限固定 1000ms）后系统静默摘除钩子且**不通知应用**，"检测钩子是否存活"没有直接 API。可行方案：同时注册 Raw Input（`RIDEV_INPUTSINK`，系统不会移除它）作为参照源；看门狗线程发现 Raw Input 持续有事件、而钩子超过 2s 无事件 → 判定钩子已被摘除 → `UnhookWindowsHookEx` + 重新 `SetWindowsHookExW`。钩子回调内只做"入队 + 返回"，任何日志/网络操作都放到工作线程。
2. **钩子线程模型**：钩子必须装在带消息循环（`GetMessage`）的专用线程；事件经无锁 ring buffer 交给 tokio 侧。
3. **绝对注入公式**（混合 DPI 多显示器，TECH-REVIEW C-2）：进程声明 PMv2（`SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2)`）；注入用 `MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK`，坐标归一化：
   `nx = (x - SM_XVIRTUALSCREEN) * 65535 / SM_CXVIRTUALSCREEN`（y 同理）。
4. **UAC 安全桌面**：普通用户进程的钩子与 SendInput 在安全桌面均失效；经 `WTSRegisterSessionNotification` 检测会话/桌面切换并在对端 UI 提示"请在目标电脑上完成 UAC 操作"。（以 Windows 服务 + SYSTEM 上下文注入安全桌面技术上可行——Synergy "Elevate: Always" 即此方案——列入 Roadmap v1.2，v1.0 不做。）
5. **防火墙（TECH-REVIEW B-5）**：MSI 安装时添加入站规则：UDP 5353（mDNS）+ 应用 QUIC 监听端口；发现失败 10s 后 UI 引导手动输入 IP。
6. **无物理鼠标时光标不可见**（lan-mouse 已知问题 C-5）：Windows 在无指针设备时默认不绘制光标；列入 FAQ，v1.x 评估规避方案。

### 3.2 macOS

| 能力 | API | 说明 |
|---|---|---|
| 键鼠捕获 | `CGEventTapCreate`（`kCGHIDEventTap`，`kCGEventTapOptionDefault`） | default(可拦截) tap 回调返回 NULL 即吞掉事件实现 grab；触发"**辅助功能**"授权（辅助功能权限涵盖输入监控，只需引导这一个权限，见 TECH-REVIEW C-3） |
| 注入 | `CGEventPost(kCGHIDEventTap, ...)` | 键盘事件携带 CGKeyCode；同样依赖"辅助功能"权限 |
| 鼠标移动 | `CGEventPost` 移动事件 + `CGWarpMouseCursorPosition` 回中心 | warp 后系统默认**抑制硬件事件 0.25s** 造成卡顿：创建专用 `CGEventSource` 并 `CGEventSourceSetLocalEventsSuppressionInterval(src, 0)`，或 warp 后立即 `CGAssociateMouseAndMouseCursorPosition(true)`（TECH-REVIEW B-1） |
| 光标隐藏 | 私有 API 规避，改用 `NSCursor.hide` + 限制在中心 | App Store 外分发可酌情用 `CGDisplayHideCursor` |
| 剪贴板 | `NSPasteboard`（轮询 `changeCount`，200ms） | 类型：`public.utf8-plain-text`、`public.html`、`public.png`/`public.tiff`；检查 `org.nspasteboard.ConcealedType` 跳过机密内容 |
| 屏幕信息 | `NSScreen` / `CGGetActiveDisplayList` | 坐标系 y 轴向上，与 Windows 相反，统一转换为"左上原点"内部坐标 |
| 权限检测 | `AXIsProcessTrustedWithOptions`、`IOHIDCheckAccess` | 引导页轮询检测，授权后无需重启即生效（tap 重建） |
| 开机自启 | `SMAppService.mainApp.register()` | macOS 13+ 官方推荐 |
| 凭据存储 | Keychain Services | |

**注意事项（按重要性排序）**：

1. **tap 被禁用的两种事件都要处理（TECH-REVIEW A-2）**：回调超时触发 `kCGEventTapDisabledByTimeout`；**睡眠/唤醒等场景触发 `kCGEventTapDisabledByUserInput`**。两种事件在 tap 回调中都会以特殊事件类型送达，收到即调用 `CGEventTapEnable(tap, true)` 重启并记日志。若 1 分钟内被禁用 >3 次，说明回调阻塞，停止重试并报错——避免死循环。
2. **权限看门狗（TECH-REVIEW A-1，最高优先级安全措施）**：每 2s 调用 `AXIsProcessTrusted()`；运行中权限被撤销时，若不处理，grab 仍在拦截而注入已失效 → **用户 Mac 整机键鼠冻结**（Deskflow #9562 实锤）。检测到撤销立即执行固定顺序：停止 grab → `release_all()` → 禁用 tap → 系统通知。与断连回退共用同一"安全回退"函数。
3. **安全输入模式**：`IsSecureEventInputEnabled()` 为 true（密码框等）时键盘注入无效，在控制端 UI 提示原因而非静默失败。
4. **坐标系**：CG 全局坐标以主屏左上为原点 y 向下，`NSScreen` y 向上；平台层统一换算成内部"左上原点"坐标，换算只在平台层做。
5. **剪贴板轮询**：`NSPasteboard.changeCount` 200ms 轮询（无变更通知 API）；读取前检查 `org.nspasteboard.ConcealedType` 跳过机密内容。
6. **开发期 TCC 权限竞态（TECH-REVIEW B-7）**：辅助功能授权与代码签名绑定，每次重编译的 ad-hoc 签名不同会导致授权反复失效甚至假授权。本地构建必须固定使用同一 Developer ID 证书签名；异常时 `tccutil reset Accessibility <bundle-id>` 重置后重新授权。

### 3.3 键码与修饰键映射

- 内部统一使用 **USB HID Usage ID** 作为中立键码；两端各自维护 `平台键码 ↔ HID` 双向表。
- 修饰键默认按物理位置映射：`Win ↔ Cmd`、`Alt ↔ Option`、`Ctrl ↔ Ctrl`；用户可在设置中改为按功能映射（如 `Ctrl ↔ Cmd`，让 Ctrl+C 在 Mac 变 Cmd+C），映射在**注入端**完成。
- 特殊键单独处理：CapsLock（macOS 有延迟特性）、Fn（不可捕获，文档声明）、媒体键（P1）。
- **布局一致性限制（TECH-REVIEW B-3）**：按位置转发要求两端键盘布局一致（如均为 QWERTY），否则 AZERTY/Dvorak/死键布局下输出错字。v1.0 在配对握手中交换布局标识（Windows `GetKeyboardLayout` / macOS `TISCopyCurrentKeyboardInputSource`），不一致时设置页常驻警告；布局感知映射（keycode→字符→反查）列入 v1.1。

---

## 4. 网络协议设计

### 4.1 连接拓扑与流划分（QUIC）

| QUIC 流 | 类型 | 内容 | 特性 |
|---|---|---|---|
| Stream 0 | 双向可靠 | 控制消息：握手、配对、布局同步、控制权移交、心跳 | 低频，必达 |
| Datagram | 不可靠 | 鼠标移动事件（仅 move，**绝对影子坐标 + 序号**） | 丢帧只丢中间轨迹点，最终位置不受影响；避免重传引入延迟尖峰 |
| Stream 1 | 双向可靠 | 键盘、鼠标按键、滚轮 | 必达且有序（按键成对性） |
| Stream 2 | 双向可靠 | 剪贴板数据 | 按需建立 |
| Stream 3+ | 单向可靠 | 文件传输（每文件一流） | 大流量与键鼠流隔离 |

> **流隔离的边界**：上面的多路复用只解决 **MouseShare 自身内部**的抢占（如传文件不卡鼠标），
> 管不到 **MouseShare 之外**的流量（别的软件下载、别的设备抢网）——那些不在本连接里。
> 后者靠下面的 DSCP 标记缓解。

#### 4.1.1 QoS / DSCP 标记（NFR-08）

目标：让路由器 QoS / Wi-Fi WMM 把 MouseShare 的小而急的控制包**插队**到下载等大流量前，
缓解"下载/多设备抢网时鼠标卡顿"。鼠标控制带宽极小（~十几 KB/s），瓶颈是**排队延迟**
（bufferbloat/空口竞争），故方案是给全部 QUIC 包打 DSCP=EF(46) 优先级标记。

实现要点（**踩坑见下，不可简单 set_tos**）：

- **quinn 不支持 DSCP**：quinn 0.11（直到 main）`Transmit` 只有 `ecn`、无 DSCP；且
  `quinn-udp` 每次发包用 cmsg 把整个 IP_TOS 写成 ECN 值，会**覆盖** socket 级设的 DSCP。
  故**不能**用默认 socket + setsockopt。
- **自定义 socket**：`ms-net::qos::DscpUdpSocket` 实现 `quinn::AsyncUdpSocket`，直接用
  tokio UDP socket 收发单个数据报（不走 cmsg、不做 GSO/GRO/ECN），DSCP 在建 socket 时
  `socket2::set_tos` 设一次长期生效。经 `Endpoint::new_with_abstract_socket` 注入 quinn。
  代价（失去 GSO/GRO/ECN）对本项目极小流量可忽略。
- **平台差异**：macOS/Linux `set_tos` 直接生效；**Windows 默认忽略 setsockopt(IP_TOS)**，
  须经 qWAVE（`ms-platform-windows::qos`，连接建立后把 socket 挂到 `QOSTrafficTypeVoice`
  flow，自动 EF、免管理员），由 daemon 在每次连接后挂、断连随 `QosFlow` Drop 释放。
- 标记失败一律降级（连接继续、仅无 QoS），不阻断互控。

### 4.2 消息格式

`[u8 类型][u16 长度][bincode 载荷]`，所有消息定义于 `proto` crate：

```rust
pub enum Msg {
    Hello { proto_ver: u16, device_id: Uuid, name: String, app_ver: String },
    PairRequest { cert_fingerprint: [u8; 32] },
    PairConfirm { sas_verified: bool },          // 配对码人工确认结果
    ScreenInfo { virtual_rect: Rect, scale: f32 },// 虚拟屏幕尺寸，布局计算用
    LayoutUpdate { layout: Layout },              // 任一端修改布局即广播
    EnterScreen { entry_edge: Edge, ratio: f32 }, // 控制权移交：从哪条边、比例位置进入
    LeaveScreen,                                  // 控制权归还
    KeyEvent { hid: u16, down: bool, shadow: (f32, f32) },   // 冗余携带影子坐标
    MouseButton { btn: u8, down: bool, shadow: (f32, f32) }, // 注入前先校正位置
    MouseMove { seq: u32, x: f32, y: f32 },       // 绝对影子坐标（Datagram 通道）
    Scroll { dx: f32, dy: f32, precise: bool },
    ClipboardData { mime: String, total: u32, chunk: Bytes },
    FileOffer { id: Uuid, name: String, size: u64 },
    FileAccept { id: Uuid } / FileChunk { .. } / FileDone { .. } / FileCancel { .. },
    Ping { seq: u32, t0: u64 } / Pong { seq: u32, t0: u64 },
    ReleaseAll,                                   // 显式要求对端释放按键
}
```

版本协商：`Hello.proto_ver` 不一致时取交集能力运行；不兼容则提示升级（为后续版本留余地）。

### 4.3 坐标映射

- 每端把自己的多显示器并集抽象为 `virtual_rect`，以左上为原点、物理像素为单位上报。
- 切换时只传 `(entry_edge, ratio)`：ratio = 离开点在源屏幕该边上的归一化位置（0.0–1.0），目标端按自己边长换算进入坐标 → 天然解决分辨率/DPI 差异（FR-203）。
- **影子光标模型（TECH-REVIEW A-4 修正）**：REMOTE 期间，控制端用本机原始相对位移（Raw Input / tap 的 delta）累积维护一个**影子光标位置**（被控端虚拟屏幕坐标系，控制端做 clamp），Datagram 发送 `MouseMove{seq, x, y}` 绝对坐标。
  - 被控端维护 `last_seq`，丢弃 `seq <= last_seq` 的乱序帧；丢包只丢中间轨迹点，最终位置永远正确，**无累积漂移**。
  - 按键/滚轮消息（可靠流）冗余携带当前影子坐标，被控端注入按键前先把光标校正到该位置，保证"先到位、再点击"。
  - 边缘判定由**控制端**基于影子坐标完成（命中被控端虚拟屏幕边界 → 发 LeaveScreen 收回控制权），被控端无需上报位置。
  - 发送频率：跟随原始事件（典型 125–1000Hz），可合并同一轮询周期内的多个 delta；seq 溢出按环形比较处理。

### 4.4 心跳、断连与重连状态机（FR-6xx 核心）

```
DISCOVERING ──发现/被发现──▶ CONNECTING ──TLS+配对校验──▶ CONNECTED
     ▲                          │失败,退避重试                │
     │                          ▼                            │心跳3次超时/网络错误
     └──── mDNS 周期重查 ◀── RECONNECTING ◀──────────────────┘
                                （0.5s→1→2→4→…→10s 指数退避，无限重试）
```

- 心跳：CONNECTED 态每 1s 一个 `Ping`（走 Stream 0），3s 无 `Pong` 判定异常；同时利用 QUIC 自身的 keep-alive 与 idle timeout（设 8s）双保险。
- RTT 由 Ping/Pong 计算，用于 UI 延迟显示（FR-606）。
- 断连即刻动作（顺序固定）：① 控制权强制回 LOCAL，解除 grab/confine；② 本端 `release_all()`；③ 进入 RECONNECTING；④ 托盘状态更新。
- IP 变更：QUIC 连接迁移可无感存活；若迁移失败则回到 mDNS 重新发现（FR-603）。
- 睡眠/唤醒：监听系统电源事件（Windows `WM_POWERBROADCAST` / macOS `NSWorkspace` 通知），睡眠前主动发"挂起"通知，唤醒后跳过退避立即重连（FR-604 ≤5s）。

### 4.5 安全设计

- 双端各自生成长期自签 Ed25519 证书，QUIC/TLS 1.3 双向认证（mTLS）。
- 首次配对（防中间人）：TLS 建立后，双端用 `导出的 TLS 会话密钥 + 双方证书指纹` 计算 6 位短认证串（SAS）显示给用户，两边一致且用户确认才写入信任列表（同 BLE 配对思路）。
- 之后每次连接校验对端证书指纹必须在信任列表中（pinning），否则拒绝（FR-702）。
- 私钥、信任列表经 DPAPI / Keychain 加密存储（FR-703）。

---

## 5. 关键流程时序

### 5.1 边缘切换（A → B）

```
A(控制端)                                B(被控端)
│ 鼠标命中右边缘+停留阈值                  │
│ grab=true, confine, 隐藏光标            │
│ ──── EnterScreen{Right→Left, 0.42} ───▶│ 光标置于左边缘 42% 处，显示
│ ──── MouseMove(seq,x,y)… (Datagram) ──▶│ 按 seq 去旧后注入绝对位置
│ ──── KeyEvent/Scroll… (Stream1) ──────▶│ 注入
│                                        │ 光标命中左边缘(返回方向)
│ ◀──────────── LeaveScreen ─────────────│
│ 解除grab，光标在右边缘对应位置出现        │ release_all()
```

### 5.2 剪贴板同步

切换控制权时（EnterScreen/LeaveScreen 之后），源端读取剪贴板 changeCount/序号（Windows 用 `GetClipboardSequenceNumber`），若自上次同步后有变化且未标记机密、未超限，则压缩（zstd）分块发送；目标端写入系统剪贴板。

**跨平台格式转换规则（TECH-REVIEW B-6，协议内统一为中立格式）**：

| 中立格式（协议传输） | Windows 侧 | macOS 侧 |
|---|---|---|
| `text/plain; utf-8` | `CF_UNICODETEXT`（UTF-16 互转） | `public.utf8-plain-text` |
| `text/html`（**纯 HTML fragment**） | `CF_HTML`：接收时生成 `Version/StartHTML/EndHTML/StartFragment/EndFragment` 头部封装；发送时按头部字节偏移提取 fragment（注意偏移按 UTF-8 字节计） | `public.html`（原样） |
| `image/png` | `CF_DIBV5` ↔ PNG 转码（注意 DIB 行序：biHeight>0 为自下而上；V5 含 alpha 掩码） | `public.png`（原样；若源只有 TIFF 则转 PNG） |

写入目标剪贴板时一次性放入该平台的全部等价格式（如文本同时放 `CF_UNICODETEXT` 与 `CF_TEXT`），最大化粘贴兼容性。

---

## 6. 工程结构与规范

### 6.1 Cargo workspace

```
mouseshare/
├── Cargo.toml                 # workspace
├── crates/
│   ├── core/                  # 状态机、布局、会话（无平台依赖，单测主战场）
│   ├── proto/                 # 消息定义与编解码
│   ├── net/                   # QUIC 传输、mDNS 发现、重连状态机
│   ├── platform/              # trait 定义
│   ├── platform-windows/      # Win32 实现
│   ├── platform-macos/        # CoreGraphics/AppKit 实现
│   └── app/                   # 主程序：组装各层 + Tauri UI + 托盘
├── ui/                        # 设置界面前端（TS + Vite）
├── installer/                 # WiX(MSI) / create-dmg 脚本
└── .github/workflows/         # CI
```

### 6.2 编码规范

- `cargo fmt + clippy -D warnings` 强制；`unsafe` 仅允许出现在 platform-* crate，且每处必须注释安全性论证。
- 事件热路径（捕获回调 → 队列 → 网络发送）禁止分配与锁竞争：使用预分配 ring buffer + `crossbeam` 无锁通道。
- 所有断连/重连路径必须打 `tracing` 结构化日志（含原因码），日志滚动保留 7 天。

---

## 7. 构建、签名与分发

| 平台 | 产物 | 要点 |
|---|---|---|
| Windows | MSI（WiX v4） | Authenticode 代码签名（EV 证书避免 SmartScreen 拦截）；x64 + ARM64 |
| macOS | .dmg（Universal 2 二进制） | Developer ID 签名 + 公证（notarytool）；Hardened Runtime；首次启动权限引导页 |
| CI | GitHub Actions | PR：fmt/clippy/单测双平台矩阵；tag：自动构建签名产物 + 自动更新 feed |
| 自动更新 | Tauri updater（或 Sparkle/WinSparkle） | 双端版本不一致时托盘提示 |

---

## 8. 测试策略

| 层级 | 内容 |
|---|---|
| 单元测试 | core/proto：坐标映射边界值（DPI 组合矩阵）、控制权状态机全转移路径、消息编解码 round-trip、重连退避序列 |
| 集成测试 | net：本机双进程模拟两端，注入丢包/延迟/断流（toxiproxy 或 quinn 测试钩子），验证心跳判定与恢复时间 |
| 端到端 | 双真机脚本化测试：使用硬件按键注入器或对端自动化驱动，跑验收标准 §6 全部场景 |
| 稳定性专项 | 72h 浸泡测试：循环执行 切换/打字/剪贴板/睡眠唤醒/Wi-Fi 通断，监控内存、钩子存活、tap 存活、恢复时间分布 |
| 兼容性矩阵 | Win10/Win11 × macOS 13/14/15 × Intel/AS × 100%/150%/200% 缩放 × 单/多显示器 |
| 安全测试 | 抓包验证密文；伪造设备连接被拒；中间人场景 SAS 不一致 |

---

## 9. 里程碑计划（单人全职估算，合计约 16 周）

| 阶段 | 周期 | 交付物 | 出口标准 |
|---|---|---|---|
| M0 骨架 | 1 周 | workspace、CI、平台 trait、双平台空实现可启动 | CI 双平台绿 |
| M1 输入打通 | 3 周 | Windows/macOS 捕获+注入；本机回环验证 | 本机注入回放一段录制输入无误 |
| M2 网络与配对 | 2 周 | QUIC+mTLS、mDNS 发现、SAS 配对、心跳重连 | 双机配对、断网自动恢复 ≤5s |
| M3 核心体验 | 3 周 | 边缘切换、坐标映射、修饰键映射、release_all | 验收标准 1–3 通过 |
| M4 剪贴板 | 1 周 | 文本/HTML/图片同步 | 验收标准 4 通过 |
| M5 文件传输 | 2 周 | 拖拽/粘贴式传输、进度 UI | 1GB 文件传输成功可取消 |
| M6 UI 与系统集成 | 2 周 | 设置界面、托盘、权限引导、自启、热键 | 全功能可经 UI 配置 |
| M7 稳定性与发布 | 2 周 | 浸泡测试、签名公证、安装包、文档 | 验收标准 5–6 通过，发布 v1.0 |

---

## 10. 风险清单

| 风险 | 等级 | 缓解措施 |
|---|---|---|
| macOS 权限运行时被撤销 → 整机输入冻结 | **极高** | 权限看门狗 2s 轮询 `AXIsProcessTrusted`，撤销即走安全回退（§3.2 注意事项 2；Deskflow #9562 教训） |
| macOS EventTap 被系统禁用（Timeout 与 UserInput 两种） | 高 | 监听两种 Disabled 事件自动重启 tap；回调内零阻塞；频繁禁用熔断；浸泡测试覆盖 |
| Windows 低级钩子被系统摘除（静默、无通知） | 高 | 以 Raw Input 为参照源的看门狗自动重装（§3.1 注意事项 1）；回调 <1ms |
| 两端键盘布局不一致输出错字 | 中 | 握手交换布局标识并警告；v1.0 限定布局一致，v1.1 做布局感知映射 |
| Windows 防火墙拦截 mDNS/QUIC | 中 | 安装器写入防火墙规则；发现失败引导手动 IP |
| macOS 新版本收紧权限/API 行为变化 | 中 | 每个 macOS beta 周期回归测试；权限检测与引导自动化 |
| QUIC(UDP) 在部分路由器/企业网受限 | 中 | Transport trait 预留 TCP+TLS 回退实现 |
| 原生文件拖拽（drag-out 时序）实现复杂 | 中 | v1.0 采用"拖到边缘弹确认/复制粘贴"兜底交互（FR-502），原生体验放 v1.1 |
| 输入法/安全输入边缘场景 | 中 | 物理键转发策略 + 明确提示；建立 IME 测试用例集 |
| 双端版本不一致导致协议错位 | 低 | Hello 版本协商 + 能力交集运行 + 更新提示 |

---

## 11. 开发环境与上手指南

### 11.1 环境准备

| 项 | Windows 开发机 | macOS 开发机 |
|---|---|---|
| 工具链 | Rust stable + MSVC Build Tools；WiX v4 | Rust stable + Xcode CLT；create-dmg |
| 前端 | Node 20 + pnpm（Tauri UI） | 同左 |
| 签名 | 开发期可不签；发布需 Authenticode 证书 | **必须固定证书签名**（哪怕自建开发证书），否则 TCC 权限每次编译后失效（§3.2 注意事项 6）；权限错乱时 `tccutil reset Accessibility <bundle-id>` |
| 真机 | 必备。输入捕获/注入无法在 CI 或虚拟机中可靠验证（VM 的输入栈不同） | 必备，同左 |

### 11.2 单机开发与调试技巧

- **回环模式**：`app --loopback` 在本机同时跑两个实例（不同端口、互为对端），核心层/协议层/网络层全链路可单机调试；仅平台 grab/注入需双真机。
- **事件录制回放**：platform 层提供 `--record events.jsonl` 与 `--replay events.jsonl`，把捕获事件序列化落盘；M1 的出口标准（注入回放无误）即基于此工具。
- **模拟弱网**：集成测试用 quinn 的 test utils 或本机 `tc`/`pfctl` 注入延迟丢包；重连测试直接 kill 对端进程/禁用网卡。
- **日志**：`RUST_LOG=mouseshare=debug` 输出结构化日志；所有断连路径必须能从日志还原原因码（reason code 枚举见 net crate）。

### 11.3 各模块完成定义（DoD）

| 模块 | 完成标准（除单测全绿外） |
|---|---|
| platform-windows 捕获 | 录制 1 分钟混合输入（打字/拖拽/滚轮/侧键），事件数与 Raw Input 参照一致；钩子被人为摘除后 ≤2s 自动重装 |
| platform-windows 注入 | 回放录制序列，记事本/浏览器中结果与原始操作一致；150% 缩放副屏上点击坐标误差 ≤1px |
| platform-macos 捕获 | grab 模式下本机无响应、事件全部入队；睡眠唤醒后 tap 自动恢复；撤销辅助功能权限 ≤2s 内安全回退 |
| platform-macos 注入 | 回放一致；warp 回中心期间移动无 0.25s 卡顿 |
| net | 回环模式下：心跳断定 ≤3s、重连恢复 ≤5s（含睡眠唤醒模拟）、10% 丢包下鼠标最终位置零漂移 |
| core | 状态机全转移路径属性测试（proptest）通过；DPI 组合矩阵（100/125/150/200%²）坐标映射边界值通过 |
| 剪贴板 | 三类格式双向 round-trip 字节级正确；CF_HTML 偏移含中文（多字节）用例通过 |

### 11.4 新人上手路径（建议顺序）

1. 读 REQUIREMENTS.md §3.2/3.3/3.6（核心行为定义）→ 本文档 §2（架构）→ §4（协议）。
2. 跑通回环模式，用 `--replay` 观察事件流日志。
3. 从 core crate 的单测入手改代码（无平台依赖，反馈最快）。
4. 涉及平台层时，先读 §3 对应平台的"注意事项"清单与 TECH-REVIEW.md 对应条目——每条都是真实项目踩过的坑。

---

## 12. 参考资料

- **TECH-REVIEW.md（本仓库）**：自检报告，含全部坑点的证据来源链接，动手前必读。
- Deskflow（GPL-2.0，仅参考架构思路）: https://github.com/deskflow/deskflow ——其 issue 区是免费的错题本（#9562 权限冻结、#9779 光标跳变、#8249/#8263 睡眠断连）
- Input Leap: https://github.com/input-leap/input-leap
- lan-mouse（Rust 同类项目，可参考平台层实现）: https://github.com/feschber/lan-mouse
- windows-rs 文档、Apple Quartz Event Services 文档、quinn 文档
- CF_HTML 格式规范: https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format
