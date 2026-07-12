# MouseShare — AI 自主开发守则

> 本文件是 AI（Claude Code 等）进入本项目的**第一入口**。人类只负责权限确认与少量物理操作，
> 其余开发工作由 AI 按本守则自主完成。

## 0. 文档索引（按阅读顺序）

| 文件 | 作用 | AI 何时读 |
|---|---|---|
| CLAUDE.md（本文件） | 工作守则、决策权限、人工介入点 | 每个会话开始 |
| TASKS.md | 任务分解与状态追踪（唯一进度真相源） | 每个会话开始，领取任务前 |
| SPEC.md | 固化的工程契约：命名/端口/路径/消息编号/错误码/依赖基线 | 写任何代码前 |
| REQUIREMENTS.md | 需求（FR-xxx/NFR-xx 编号引用） | 实现某功能时查对应条目 |
| DEVELOPMENT.md | 架构、平台实现要点、协议设计 | 实现某模块时查对应章节 |
| TECH-REVIEW.md | 踩坑证据库（A/B/C 编号引用） | 触碰平台层代码前必读对应条目 |

## 1. 会话工作流（每次必须遵守）

1. 读 TASKS.md，找到状态为 `进行中` 的任务；若无，按依赖顺序领取第一个 `待办` 任务并改为 `进行中`。
2. 读该任务引用的 FR/章节/坑点条目，再写代码。
3. 完成后**必须**执行任务表中"验证"列的命令，全部通过才能把状态改为 `已完成`。
4. 验证含 `[HUMAN]` 标记的步骤：把状态改为 `待人工`，在任务行"备注"列写清楚**需要人做什么、怎么判断通过**，然后停止该任务（可继续领取无依赖冲突的其他任务）。
5. 每完成一个任务提交一次 git commit（规范见 SPEC.md §7）。禁止把多个任务混进一个 commit。
6. 每完成一个任务/关键操作/异常，在 OPLOG.md 末尾追加一行操作日志（时间、任务、摘要、结果、commit）。
7. 文件边界：所有开发产物只写入项目目录（含 `.git/`、`target/`、`logs/`）；开发期运行程序一律加 `--log-dir ./logs`，避免写到 SPEC §4 的系统日志路径。
8. 会话结束前：确保 TASKS.md 状态与实际代码一致；未完成的工作在任务"备注"列写明断点（做到哪、下一步是什么），让下一个会话能无缝接续。

## 2. 决策权限矩阵

| 类别 | AI 可自主决定 | 必须先问人 |
|---|---|---|
| 代码实现细节（函数拆分、命名、内部数据结构） | ✅ | |
| 添加 SPEC.md §6 基线内已列出的依赖 | ✅ | |
| 添加基线外的新依赖 | | ⚠️ 说明理由后等确认 |
| 修改 SPEC.md 中的契约（端口、消息编号、路径等） | | ⚠️ 这是跨会话一致性的根基 |
| 修改 REQUIREMENTS.md 的功能范围/验收标准 | | ⚠️ 需求变更必须人确认 |
| 偏离 DEVELOPMENT.md 的架构（如换传输层实现） | | ⚠️ 给出对比分析后等确认 |
| 在 TASKS.md 中拆分/细化任务（不改里程碑出口标准） | ✅ 但要在备注注明拆分原因 | |
| 删除/重写已通过验证的旧代码 | ✅ 若为当前任务所需重构 | 大面积重写（>3 个文件）先说明 |
| 发布操作（git tag、上传产物、对外发包） | | ⚠️ 永远需要人确认 |
| 联网调研 API/crate 文档 | ✅ | |

**遇到文档间矛盾**：以 SPEC.md > REQUIREMENTS.md > DEVELOPMENT.md 的优先级处理，同时把矛盾点报告给人并在文档中修复。

## 3. 人工介入点（AI 物理上做不到的事）

以下场景在 TASKS.md 中以 `[HUMAN]` 标记，AI 到达时必须暂停并给出明确操作指引：

| 编号 | 场景 | 人需要做的事 |
|---|---|---|
| H-1 | macOS TCC 授权 | 在"系统设置→隐私与安全性→辅助功能"中勾选应用；权限异常时执行 AI 给出的 `tccutil reset` 命令 |
| H-2 | 双真机测试 | 按 AI 给出的步骤在 Windows 机上安装/运行/操作，回报现象或日志 |
| H-3 | 签名证书 | 提供/创建 Developer ID 证书（macOS）与 Authenticode 证书（Windows，发布期才需要） |
| H-4 | 配对码确认 | 双机配对时人工核对两端 6 位码一致 |
| H-5 | 主观体验验收 | "光标移动是否平滑/有无卡顿"等无法用命令断言的体验项 |
| H-6 | 工具权限确认 | Claude Code 弹出的命令执行授权 |
| H-7 | 发布确认 | tag、公证、上传安装包 |

**当前开发环境假设**：主开发机为 macOS（本机）。Windows 侧代码先保证交叉编译通过，真机验证统一归入 H-2 批次执行，不阻塞后续任务。

**Windows 交叉编译（mac→Windows）**：用 **cargo-zigbuild**（zig 提供 C 工具链，能编 `ring`/`zstd-sys` 等含 C/汇编的依赖），在 mac 上即可产出完整 Windows exe，**不**必等真机编译。

- 完整 app（出 exe）：`cargo zigbuild --release --target x86_64-pc-windows-gnu -p ms-app`
  → 产物 `target/x86_64-pc-windows-gnu/release/mouseshare.exe`
- 纯逻辑快检（不出 exe，更快）：`cargo check --target x86_64-pc-windows-gnu -p ms-core -p ms-proto -p ms-platform -p ms-platform-windows`
- **目标三元组固定用 `-gnu`**：zig 自带 mingw libc 头文件，开箱即用；`-msvc` 需额外下载 Windows SDK 头文件（xwin），本项目不用——误用会报 `'string.h' file not found`（2026-06-29 踩过：错选 msvc 目标导致 zstd 的 C 代码编译失败，与 zig 版本无关）。
- 前置：本机需 `zig` + `cargo-zigbuild`（`brew install zig` + `cargo install cargo-zigbuild`）。

app/网络层的**真机功能验证**仍归 H-2 批次（T-501/T-701）；但"能否交叉编译通过"在 mac 上用上面命令即可验证，不再是真机才能做的事。

## 4. 验证协议（"完成"的定义）

- **每个任务的最低门槛**：`cargo fmt --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace` 三连全绿。（UI 为 egui + tray-icon 纯 Rust，无前端工程；UI 任务的额外验证是 `cargo build -p ms-app` 通过，**不涉及 pnpm/npm**。）
- **平台相关代码**：macOS 侧本机实测；Windows 侧编译通过 + 单测（可在 mac 上跑的逻辑部分）通过即可标 `已完成`，并自动在对应的 H-2 批次任务中登记待真机验证项。
- **禁止**：跳过验证直接标完成；为让测试通过而放宽断言/删除测试；用 `#[ignore]` 掩盖失败（确需 ignore 必须在备注说明原因与恢复条件）。
- 回归保障：修一个 bug 必须先写复现测试（能写的话），再修。

## 5. 编码红线（摘要，详见 SPEC.md）

- `unsafe` 只允许出现在 `platform-windows` / `platform-macos`，每处必须有 `// SAFETY:` 论证。
- 事件热路径禁止堆分配与锁（DEVELOPMENT.md §6.2）。
- 所有断连/回退路径必须打带 reason code 的 tracing 日志（枚举见 SPEC.md §5）。
- 安全回退顺序（停 grab → release_all → 停捕获 → 通知）是**不可重排的事务**，实现处必须注释引用 TECH-REVIEW A-1/B-2。
- 参考 Deskflow（GPL）只看思路，**禁止复制任何代码**；lan-mouse（GPL-3.0 同理）。
