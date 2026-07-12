# 会话交接（HANDOFF）

> 给下一个 session 的冷启动快照。**真相源仍是 TASKS.md / OPLOG.md**，本文件只做"当前状态 +
> 待办 + 下一步"的提炼。入口规则见 CLAUDE.md。**最近更新：2026-06-20（HEAD=97d4d05）**。

## 0. 怎么接手（30 秒上手）

1. 读 CLAUDE.md（工作守则）→ 本文件 → TASKS.md（找 `进行中`/`待办`）。
2. 三连验证门槛：`cargo fmt --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`。
3. mac 真机相关：`cargo test -p ms-platform-macos -- --ignored`；Windows 交叉编译：`cargo zigbuild -p ms-app --target x86_64-pc-windows-gnu`。
4. 打包：`cargo build --release -p ms-app && bash scripts/build-macos-app.sh`（mac）；
   `cargo zigbuild --release -p ms-app --target x86_64-pc-windows-gnu && cp target/x86_64-pc-windows-gnu/release/mouseshare.exe dist/`（win）。
5. 每任务一次 commit（SPEC §7 格式 `<type>(<scope>): 摘要 (T-xxx)` + Co-Authored-By）；OPLOG 追加一行。

## 1. 一句话现状

键鼠互控 / 剪贴板三类 / 文件传输（进度·取消·大文件确认·传输日志面板）/ 双击 / 自动重连 /
设置即时生效 / mac 权限引导 / **强制退出(⌘⌥⎋)** 均已实现，**mac 侧大多真机验证通过**。
**当前 3 个待用户验证 + 1 个待查 bug + 2 个下一步功能**（见 §3、§4）。
**架构注**：文件传输已搬回开源核心 `ms-net::transfer`，默认即用（付费/分发模型暂缓，见 SPEC §7）。

## 2. 已完成且真机验证通过（用户确认）

- 双机穿越 / 组合键(Ctrl↔Cmd) / 逃生 / 单向主从（T-308/T-501）
- 剪贴板**文本/图片/HTML** 三类双向（T-405，验收标准4 ✅）
- 文件传输 **Windows→mac** 方向 + 大文件发送(>1GB 确认) + 进度/传输日志面板（T-504/T-506）
- mac 被控端**双击打开**文件/文件夹（T-309）

## 3. ⚠️ 待用户验证 / 待查（下个 session 优先跟进）

### 3a. T-507 — mac→Windows 文件传输不通（**待查，最高优先级**）
- 现象：Win→mac 文件正常；**mac→Win 文件收不到**，mac 日志显示 `读到剪贴板文件引用数 files=0`。
- **已排除**：read_files 读取格式没坏——本机用真实 `public.file-url` 剪贴板测试，read_files 正确读出路径
  （见 platform-macos/clipboard.rs 的 `调试_打印剪贴板` ignored 测试，commit c14b041）。
- **结论**：问题是**切换那一刻 mac 剪贴板里没有那个文件**（不是读取 bug）。
- **下一步（等用户反馈）**：让用户验证「mac Finder 复制文件后，先在 mac 本地 Cmd+V 能否粘出」：
  - 粘不出 → 被控态下注入的 Cmd+C 对 Finder 文件未生效 → 查注入/焦点；
  - 能粘出 → 穿越时机问题 → 查 on_transition 读取时序（clip.rs `on_transition` 在 mac `Controlled→Local` 或 `Local→Remote` 时读 read_files）。

### 3b. T-510 — 强制退出(⌘⌥⎋) 仍要按 3 次（**待用户更新 Windows 端复测**）
- 已修：① mac inject 检测 ⌘⌥⎋ → osascript 经 System Events 打开强制退出面板（绕过 macOS 对合成系统热键的过滤，TECH-REVIEW B-9）；② **紧急逃生只认"裸 Esc"三连**，带修饰键的 Esc 不计入（修与 ⌘⌥⎋ 的冲突）。
- **关键**：逃生/重连逻辑跑在**控制端**（用户是 Win 控 mac → 在 **Windows 端**）。用户反馈"仍要 3 次"很可能因 **Windows 端没换新构建**。下个 session 先确认用户两端都更新。
- 首次触发 osascript 会弹一次「MouseShare 想要控制"系统事件"」自动化授权，需用户点允许（一次性，类 H-1）。

### 3c. T-509 — 开机自连屏幕尺寸过渡值致入点跳角落（**待用户下次开机复测**）
- 已修：`detect_own_screen` 检测到尺寸非法(w/h≤0)时重试 ~2s 再用；屏幕尺寸已记入日志。
- 开机/唤醒时序问题本机无法复现，待用户开机自连复测；日志现打 `本机屏幕尺寸已检测 w= h=`。

## 4. 下一步功能目标（**仅设计/已登记，未实现**）

### 4a. T-511 — 边缘穿越加"刻意触发"条件（**用户指定的下一目标**）
- 痛点：一碰屏幕边缘就穿越，够不着 mac 顶部菜单栏 / Windows 底部任务栏。
- 现状：`EdgeConfig.threshold` 默认 1px，`detect_exit` 命中即穿越，无停留/推挤要求。
- 方案：加**边缘停留延迟 + 推挤距离**（命中边缘先钉住、继续推 X px 才穿越，类 Synergy/Barrier），参数走 Config 可调；保留紧急逃生与 corner_dead。代码点：core `layout.rs::detect_exit` + `engine.rs::local_input`。

### 4b. T-508 — 拖拽文件穿越屏幕传输（FR-501 原生拖放）
- 设计/开发手册见 **docs/FEATURE-drag-transfer.md**（含难点、总体方案、平台 API、子任务 T-508a~f）。与已有粘贴式（T-504）共存。

## 5. 其余待办（未排期）

- T-701：Windows 真机走查批次（所有近期改动的 Win 侧：剪贴板/文件/双击/重连/设置即时生效/强制退出，均只 mac 验证过）。
- T-702 浸泡测试、T-703 安全抓包验证、T-605 国际化、T-005 CI（需远程仓库）。
- 发布相关（T-704 安装包签名 / T-705 自动更新 / T-706 v1.0）：用户已明确**暂缓**。

## 6. 速查

- 日志：mac `~/Library/Logs/MouseShare/`，Windows `%LOCALAPPDATA%\MouseShare\logs\`；默认级别 `mouseshare=info`，更细 `RUST_LOG=mouseshare=debug`。
- 文件接收目录：mac `~/Downloads/MouseShare/`，Windows `%USERPROFILE%\Downloads\MouseShare\`。
- 产物在 `dist/`（已 gitignore）：`MouseShare.app` + `mouseshare.exe`。
- 关键模块：daemon 主循环+重连/会话 `crates/app/src/daemon.rs`；剪贴板/文件接线 `clip.rs`/`xfer.rs`/`transfers.rs`；
  文件传输实现 `crates/net/src/transfer.rs`；引擎/边缘/逃生 `crates/core/src/{engine,layout,control}.rs`；
  mac 平台 `crates/platform-macos/src/{inject,clipboard,permission,screen}.rs`。
- 踩坑库 TECH-REVIEW：B-7 TCC 失效 / B-8 合成双击 clickState / B-9 合成系统热键(强制退出)被过滤。
