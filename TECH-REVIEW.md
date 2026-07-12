# MouseShare 技术自检报告

| 项目 | 内容 |
|---|---|
| 文档版本 | v1.0 |
| 日期 | 2026-06-12 |
| 性质 | 对 REQUIREMENTS.md / DEVELOPMENT.md v1.0 的联网调研自检；所列修正已同步回两份文档 |

本报告逐项列出：**问题描述 → 证据来源 → 影响 → 修正措施**。按严重程度排序。

---

## A. 高危问题（不修正会导致产品不可用或损坏用户系统体验）

### A-1 macOS 权限被运行时撤销 → 整机键鼠冻结

- **问题**：原文档只处理"首次授权引导"，未处理运行中权限被撤销的场景。Deskflow 实测（[issue #9562](https://github.com/deskflow/deskflow/issues/9562)）：若用户在 grab 生效期间于系统设置中撤销"辅助功能"权限，event tap 注入失效但 grab 仍在拦截本机输入，导致 **Mac 本机键鼠完全失灵**，用户只能强制重启。
- **影响**：最严重的故障模式——软件把用户的电脑"锁死"。
- **修正**：平台层增加**权限看门狗**：每 2s 调用 `AXIsProcessTrusted()` 检测；一旦变为 false，立即按固定顺序执行 ①停止 grab ②`release_all()` ③禁用 tap ④弹系统通知。该顺序与断连回退（FR-605）共用同一段"安全回退"代码路径，并列入浸泡测试必测项。已写入 DEVELOPMENT.md §3.2。

### A-2 macOS event tap 被系统禁用的触发条件不止 timeout

- **问题**：原文档只提到 `kCGEventTapDisabledByTimeout`。调研证实睡眠/唤醒会触发 `kCGEventTapDisabledByUserInput`（Deskflow 日志 "quartz event tap was disabled by user input"，[issue #8249](https://github.com/deskflow/deskflow/issues/8249)），不处理则唤醒后无法再切换屏幕。
- **修正**：tap 回调中对**两种**禁用事件均调用 `CGEventTapEnable` 重启，并打日志计数；连续频繁禁用（>3 次/分钟）说明回调阻塞，需上报错误而非死循环重启。已写入 DEVELOPMENT.md §3.2。

### A-3 Windows 低级钩子被静默移除且**无任何通知**

- **问题**：原文档写"监测钩子存活"，但未给出可行方法。微软文档确认：回调超时（Win10 1709+ 上限固定 1000ms）后钩子被**静默移除，应用无从得知**（[LowLevelKeyboardProc 文档](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc)）。"检测存活"本身不可实现。
- **修正**：改为**主动重装策略**——看门狗线程对比"Raw Input 收到事件的时间戳"与"钩子收到事件的时间戳"，若 Raw Input 持续有事件而钩子超过 2s 无事件，判定钩子已被摘除，执行 Unhook+重装。Raw Input 注册（`RIDEV_INPUTSINK`）不会被系统移除，天然适合做参照源。已写入 DEVELOPMENT.md §3.1。

### A-4 鼠标移动用"相对位移 + 不可靠 Datagram"会产生位置漂移

- **问题**：原协议 `MouseMove{dx,dy}` 走 QUIC Datagram。丢包 = 丢位移，累计误差使控制端"影子位置"与被控端实际光标位置逐渐失配；且 Datagram 乱序会造成轨迹抖动。
- **修正**：协议改为**绝对影子坐标**：控制端维护影子光标 `(x,y)`（被控端虚拟屏幕坐标系），Datagram 发送 `MouseMove{seq,x,y}`；被控端丢弃 seq 旧于已处理值的帧。丢包只丢中间轨迹点，最终位置永远正确，无累积误差。按键事件（可靠流）中冗余携带当前影子坐标，注入按键前先校正位置，杜绝"还没移动到位就点击"。已重写 DEVELOPMENT.md §4.2/§4.3。

---

## B. 中危问题（功能缺陷或体验损伤）

### B-1 macOS 光标 warp 后系统抑制输入 0.25s

- **问题**：原文档提到"warp 后系统会抑制一次 delta，需补偿"，不准确。实际行为：`CGWarpMouseCursorPosition` 后系统**抑制硬件事件约 0.25 秒**（[参考](https://www.colincornaby.me/2023/02/mac-games-and-cursor-warping-weirdness/)），表现为控制对端时鼠标周期性卡顿。
- **修正**：创建专用 `CGEventSource` 并调用 `CGEventSourceSetLocalEventsSuppressionInterval(src, 0)`，或 warp 后立即 `CGAssociateMouseAndMouseCursorPosition(true)`。已写入 DEVELOPMENT.md §3.2。

### B-2 切换瞬间光标跳变（Deskflow 现行 bug 的教训）

- **问题**：Deskflow [issue #9779](https://github.com/deskflow/deskflow/issues/9779)：macOS 端 leave 时未把光标 warp 回屏幕中心、未重置位置参考点，导致切回时光标跳变一段向量。
- **修正**：在控制权状态机中把"进入/退出 REMOTE"的平台动作定义为**固定顺序的事务**（见 DEVELOPMENT.md §2.3 修订）：进入 = warp 中心 → 记录参考点 → grab → 隐藏光标；退出 = 解除 grab → warp 到返回位置 → 显示光标 → 双端 release_all。热键切换与边缘切换共用同一事务代码。

### B-3 键盘布局不一致时按位置转发会输出错字

- **问题**：HID/scancode 按物理位置转发，在两端布局一致（如都是 QWERTY+简中）时正确；若一端 AZERTY/Dvorak/西语死键布局，同一物理键在两端字符含义不同（Barrier [issue #134](https://github.com/debauchee/barrier/issues/134) 即此类问题）。死键组字也依赖被控端布局。
- **修正**：v1.0 明确声明**要求两端键盘布局一致**（写入需求文档 §5 边界），设置页检测两端布局并在不一致时警告；布局感知映射（控制端 keycode→字符→被控端反查 keycode）列入 v1.1。风险表已更新。

### B-4 UAC 安全桌面"做不到"的表述不准确

- **问题**：需求文档原写"UAC 安全桌面无法接收注入输入（操作系统安全机制）"。调研发现 Synergy 通过 **"Elevate: Always"（以服务方式提权运行）** 可在 UAC 提示期间继续控制（[Synergy 官方说明](https://symless.com/synergy/help/how-to-make-synergy-work-with-uac-prompts)）。即：普通用户进程做不到，Windows 服务（SYSTEM 上下文 + `SetThreadDesktop` 切到 Secure Desktop）可以做到。
- **修正**：v1.0 维持不支持（服务架构复杂度高、安全面扩大），但需求文档 §5 改为"v1.0 不支持，可经 Windows 服务实现，列入 Roadmap v1.2 评估"，避免误导后续开发者"技术上不可能"。

### B-5 mDNS 在 Windows 上常被防火墙拦截

- **问题**：原文档未考虑 Windows 防火墙默认可能拦截 UDP 5353 入站及应用自身监听端口，导致"自动发现"在相当比例用户机器上静默失败。
- **修正**：①MSI 安装器添加防火墙入站规则（mDNS UDP 5353 + 应用 QUIC 端口）；②发现失败 10s 后 UI 主动提示切换"手动输入 IP"并给出防火墙排查指引。另注明 [mdns-sd crate](https://github.com/keepsimple1/mdns-sd) 仍标记为 beta，集成测试需覆盖。已写入 DEVELOPMENT.md §3.1/§7。

### B-6 剪贴板 HTML/图片格式互转需要明确算法

- **问题**：原文档只写了格式对应关系。实际上 Windows 的 `CF_HTML` 是带 `Version/StartHTML/EndHTML/StartFragment/EndFragment` 头部的特殊封装格式（[微软文档](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format)），与 macOS `public.html`（纯 HTML）不能直接互拷。
- **修正**：协议统一传**纯 HTML fragment（UTF-8）**：Windows→发送前按字节偏移剥离 CF_HTML 头部取 fragment；←接收时生成头部封装写回。图片统一传 PNG：Windows 端 `CF_DIB(V5)`↔PNG 双向转码（注意 DIB 自上而下/自下而上行序与 BITMAPV5 alpha），macOS 端原生支持 `public.png`。已写入 DEVELOPMENT.md §5.2。

### B-7 macOS 开发期间 TCC 权限反复失效

- **问题**：辅助功能/输入监控授权与代码签名绑定。开发中每次重新编译的 ad-hoc 签名都不同，TCC 会认为是"新程序"，导致权限反复失效甚至出现"已勾选但实际无效"的假授权状态（[参考](https://danielraffel.me/til/2026/02/19/cgevent-taps-and-code-signing-the-silent-disable-race/)）。
- **修正**：开发规范要求：本地构建统一用稳定的 Developer ID 证书（或自建开发证书）签名；权限异常时用 `tccutil reset Accessibility <bundle-id>` 重置。已写入 DEVELOPMENT.md §12 上手指南。

### B-8 macOS 合成鼠标事件不会被自动识别为双击

- **问题**：被控端用 `CGEventPost` 注入鼠标按键时，macOS **不会**像 Windows（SendInput）那样按时序自动合成双击。若不显式设置 `kCGMouseEventClickState` 字段（1=单击 / 2=双击 / 3=三击），应用只收到两次独立单击 → "双击打开文件/文件夹"等行为失效。
- **修正**：注入端（`platform-macos/inject.rs`）按"与上次同键点击的时间间隔（≤500ms）+ 位移（≤5px）"推导连击计数，在按下与对应抬起事件上设 `kCGMouseEventClickState`。因协议 `MouseButton` 未携带 clickCount，故在注入侧重新推导（捕获侧 LAN 低延迟，inter-arrival ≈ 真实间隔）。`derive_click_count` 抽为纯函数单测覆盖。

### B-9 macOS 强制退出(⌘⌥⎋)等系统符号热键不响应合成事件

- **问题**：被控 mac 用 `CGEventPost` 注入 ⌘⌥⎋ 不会弹出「强制退出应用程序」窗。普通快捷键（⌘C/⌘W，由前台 App 读事件 flags）注入有效；但 Force Quit 是系统级"符号热键"，macOS 对合成事件过滤（安全行为），不触发。
- **修正**：注入端（`platform-macos/inject.rs`）检测到 Esc 按下且按住 ⌘⌥ 时（`is_force_quit`），改经 `osascript`「tell System Events to key code 53 using {command down, option down}」触发——System Events 注入走系统认可路径，实测可弹窗。首次触发弹一次「控制"系统事件"」自动化授权(TCC Automation)，允许后长期生效（属 H-1 类一次性授权）。`is_force_quit` 抽纯函数单测覆盖。

### B-10 macOS Finder 复制文件的 `public.file-url` 是文件引用(/.file/id=)而非真实路径

- **问题**：Finder Cmd+C 复制文件时，`NSPasteboard` 的 `public.file-url`（含 `NSPasteboardItem.stringForType(NSPasteboardTypeFileURL)`）返回的是**文件引用 URL** `file:///.file/id=NNN.MMM`，**不是可直接打开的路径**。直接当路径用会：① 文件名变成 `id=NNN.MMM`（错）；② 打开该引用路径失败/读到错误数据 → 跨机传文件后**文件名与内容全错、二进制(exe)直接传不过去**。osascript 设置的 file-url 恰是真实路径形式，故早期单测未暴露。
- **修正**：`MacClipboard::read_files()` 改为**优先读 `NSFilenamesPboardType`** 属性列表——Finder 同时提供的**真实绝对路径**数组；`public.file-url` 仅作非 Finder 来源的回退。本机真实 Finder Cmd+C 实测 read_files 返回真实路径 `/Users/.../x.txt`。

### B-11 macOS WindowServer 对越界坐标的合成事件不做边缘判定（差一即失效）

- **问题**：屏幕高 H 的合法光标坐标是 `[0, H-1]`；硬件光标被系统钳在 H-1，但合成事件可携带 y=H 的越界坐标。WindowServer 会移动光标，却**不对越界坐标做「边缘唤出」判定**——自动隐藏的程序坞/全屏菜单栏等永远不出现，且无任何报错。影子光标若钳在 `[0, H]` 闭区间（差一），贴边注入即恒为 y=H → Dock「时好时坏」（偶发成功是穿越途中 H-1 过渡事件驻留凑成）。delta 字段、CGWarpMouseCursorPosition 均与此无关（T-610 两轮盲改方向全错的教训）。
- **定位方法（可复用）**：三步隔离——① 独立小工具 `CGEventPost` 贴边合成事件做变体矩阵（delta/warp/频率组合）；② listen-only CGEventTap 抓真实注入流对比（注意 macOS 15 下 CGWindowList 的 Dock owner 名是本地化的「程序坞」、bounds 被打码但 layer=20 窗口的 `kCGWindowIsOnscreen` 真实可用作 Dock 可见性判定）；③ 用差异变量做判别实验。y=900 恒失败、y=899 恒成功，一次定位（T-616）。
- **修正**：`core/layout.rs clamp_to_rect` 上界改 `w-1`/`h-1`（发端根治）；`platform-macos/inject.rs mouse_move_to` 注入前防御性钳回 `[bounds, bounds+size-1]`（对端旧版本也不复发）。

---

## C. 低危问题与确认项

| 编号 | 内容 | 处理 |
|---|---|---|
| C-1 | 技术选型表列了 tray-icon crate，实际 [Tauri 2 已内建跨平台托盘 API](https://v2.tauri.app/learn/system-tray/)，无需额外依赖 | 已修正选型表 |
| C-2 | Windows 绝对注入须用 `MOUSEEVENTF_ABSOLUTE\|VIRTUALDESK` + 0–65535 归一化到**虚拟桌面**（`SM_XVIRTUALSCREEN` 系列），且进程必须声明 PMv2 DPI aware，否则混合缩放下坐标错位（[参考](https://filipvalentin.github.io/blog/2024/08/how-to-simulate-moving-the-mouse-cursor-through-winapi-multimonitor-setup)） | 原文档方向正确，已补齐公式与细节 |
| C-3 | macOS 权限澄清：`defaultTap`（可拦截）触发**辅助功能**授权；`listenOnly` 触发**输入监控**；辅助功能权限涵盖输入监控（[Apple 论坛](https://developer.apple.com/forums/thread/122492)）。我们用 defaultTap，故实际只需引导一个权限 | 已简化 §3.2 与 FR-803 表述 |
| C-4 | [lan-mouse](https://github.com/feschber/lan-mouse)（Rust 同类项目）的 capture/emulation 后端解耦设计与我们的 trait 设计一致，可放心参考；其 macOS 修复记录（修饰键捕获、双击判定、滚轮捕获、重复 key-release）直接转化为我们的测试用例 | 已纳入测试清单 |
| C-5 | lan-mouse 已知怪问题："Windows 端无物理鼠标连接时注入的光标不可见"——Windows 在无指针设备时默认隐藏光标 | 列入已知限制与 FAQ；可经注册表/模拟设备规避，v1.x 处理 |
| C-6 | 同类软件睡眠/唤醒断连是普遍痛点（Deskflow [#8263](https://github.com/deskflow/deskflow/issues/8263)），验证了 FR-604 的必要性 | 浸泡测试已覆盖 |
| C-7 | QUIC Datagram（RFC 9221）与连接迁移在 quinn 中均受支持，选型成立 | 无需修改 |

---

## D. 仍存在的开放风险（无法靠文档消除，需原型验证）

1. **QUIC 在用户实际路由器上的表现**：部分家用路由器对 UDP 限速/丢包严重。M2 里程碑结束前必须在真实 Wi-Fi 环境做延迟分布实测，决定是否启用 TCP 回退。
2. **macOS 后续版本收紧**：Apple 每年都可能调整 TCC/事件注入行为，无法提前规避，只能在 beta 周期回归。
3. **全屏游戏/专业软件**（使用 Raw Input 或 DirectInput 的程序）对注入事件的兼容性参差，v1.0 以"暂停共享"规避，不承诺兼容。

---

## 来源汇总

- Deskflow issues: [#9562](https://github.com/deskflow/deskflow/issues/9562), [#9779](https://github.com/deskflow/deskflow/issues/9779), [#8249](https://github.com/deskflow/deskflow/issues/8249), [#8263](https://github.com/deskflow/deskflow/issues/8263)
- 微软文档: [LowLevelKeyboardProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc), [CF_HTML](https://learn.microsoft.com/en-us/windows/win32/dataxchg/html-clipboard-format), [MOUSEINPUT](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-mouseinput)
- Apple 开发者论坛: [输入权限差异](https://developer.apple.com/forums/thread/122492)
- Synergy: [UAC Elevate 模式](https://symless.com/synergy/help/how-to-make-synergy-work-with-uac-prompts)
- Barrier: [布局不一致 issue #134](https://github.com/debauchee/barrier/issues/134)
- 其他: [cursor warp 抑制](https://www.colincornaby.me/2023/02/mac-games-and-cursor-warping-weirdness/), [TCC 签名竞态](https://danielraffel.me/til/2026/02/19/cgevent-taps-and-code-signing-the-silent-disable-race/), [多显示器 SendInput](https://filipvalentin.github.io/blog/2024/08/how-to-simulate-moving-the-mouse-cursor-through-winapi-multimonitor-setup), [Tauri 2 托盘](https://v2.tauri.app/learn/system-tray/), [lan-mouse](https://github.com/feschber/lan-mouse), [mdns-sd](https://github.com/keepsimple1/mdns-sd)
