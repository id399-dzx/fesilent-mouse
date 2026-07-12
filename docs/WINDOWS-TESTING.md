# Windows 真机验证指南（H-2）

> 主开发机是 macOS，Windows 端代码在 Mac 上写好并交叉编译成 `.exe`。
> **你的 Windows 机器不需要安装任何开发工具或 Claude**——只需运行 exe 并把输出回报。

## 交叉编译命令（AI 在 Mac 上执行）

前置：`brew install zig` + `cargo install cargo-zigbuild` + `rustup target add x86_64-pc-windows-gnu`（已装）。

```
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
cargo zigbuild -p ms-app --target x86_64-pc-windows-gnu --release
```

产物：`target/x86_64-pc-windows-gnu/release/mouseshare.exe`（PE32+ console x86-64），
拷一份到 `dist/mouseshare.exe` 交付。zig 作链接器，能搞定 ring 等带 C 代码的依赖。

## 工作方式

1. AI（在 Mac 上）用 `cargo-zigbuild` 交叉编译出 `mouseshare.exe`（命令见上）。
2. 你把 exe 拷到 Windows（U 盘 / 网盘 / 局域网共享均可）。
3. 在 Windows 上打开「命令提示符」或「PowerShell」，`cd` 到 exe 所在目录运行。
4. 把屏幕上打印的结果（整段文字）复制回报给 AI。
5. AI 据此判断通过/失败并修代码，必要时给你新的 exe 重测。

## 当前可验证项

### 自检：安全存储（DPAPI）

```
mouseshare.exe --selftest
```

预期输出（PASS 表示 Windows DPAPI 加密存储工作正常）：

```
MouseShare 自检 v0.1.0
平台: windows / x86_64
------------------------------------
[PASS] 安全存储 (Keychain/DPAPI) 读写往返
------------------------------------
结果: 全部 PASS ✅
```

- 若看到 `[FAIL] ...`，把整段连同错误信息回报。
- 退出码：0 = 全过，1 = 有失败（PowerShell 里 `echo $LASTEXITCODE` 可看）。

### 键鼠捕获/注入（T-107/108）

和 mac 一样用录制/回放验证 Windows 输入层。

**1) 录制**（默认 20 秒窗口，期间打字、动鼠标、滚滚轮）：
```
mouseshare.exe --record test.jsonl --log-dir .\logs
```
看到"● 录制中"后操作键鼠。结束打印录到的事件数。把数字回报。
- 期望：事件数 > 0（比如打几个字 + 动鼠标应有几十上百个事件）。
- 若数为 0：可能被杀软拦截了低级钩子，或需要回报现象。

**2) 回放**（先点开「记事本」让焦点在输入框）：
```
mouseshare.exe --replay test.jsonl --log-dir .\logs
```
应看到刚才录的按键被"重放"到记事本里（鼠标也会动）。回报是否生效。

> 说明：低级键盘钩子行为类似"键盘记录器"，个别杀毒软件可能提示——这是你自己
> 编译的程序，允许运行即可。`test.jsonl` 是录制数据，可随意删除。

### 🖱️ 双机互控（首次真机联调）

让 mac 和 Windows 真正互相控制。两机需在**同一局域网**。

**前提**：mac 已授权终端「辅助功能」；两机防火墙允许 UDP 53728（首次可能弹窗，点允许）。

**角色**：一台 `--serve`（监听），一台 `--connect <对方IP:53728>`。下面以 **mac 监听、Windows 连接** 为例（mac IP 假设 192.168.0.104）。

`--side` = 对端在你这台的哪个方向。若物理上 Windows 在 mac **右边**：mac 用 `--side right`，Windows 用 `--side left`。`--screen`/`--peer-screen` 填各自分辨率（默认 1920x1080）。

1. **mac**（在项目目录终端）：
   ```
   target/debug/mouseshare --serve --side right \
     --screen 1920x1080 --peer-screen 1920x1080 --log-dir ./logs
   ```
2. **Windows**（PowerShell，`E:\mac\dist`）：
   ```
   .\mouseshare.exe --connect 192.168.0.104:53728 --side left `
     --screen 1920x1080 --peer-screen 1920x1080 --log-dir .\logs
   ```
3. 两端都应打印 `✓ 已连接` + **相同的配对码**（核对一致）+ `✓ MouseShare 运行中`。
4. 把 mac 鼠标移到**右**边缘 → 光标应"穿越"去控制 Windows；移回左边缘 → 回到 mac。

**⚠️ 如何夺回控制**（重要）：被控期间本机键盘被拦截。要回来：
- 把鼠标移回它"进入对端时那条边"的反方向边缘（即来时的边），控制权自动交还；
- 或直接**关掉任一终端窗口**（进程退出 → 钩子/tap 卸载 → 键鼠立刻恢复）。

> v1 是首版联调，可能有光标跳动/不够顺滑——先验证"能穿越、能打字、能回来"。
> 体验打磨（warp 补偿、延迟优化）在后续里程碑。

## 后续批次（随 Windows 功能实现逐步加入）

- T-501 批次 #1：键鼠捕获（WH_KEYBOARD_LL/WH_MOUSE_LL）、注入（SendInput）、
  钩子看门狗自动重装；双真机配对/切换/打字。
- T-701 批次 #2：剪贴板（CF_HTML/CF_DIB）、文件传输、UI、防火墙规则、安装包。

每个批次 AI 会更新本文件，给出对应的运行命令与预期现象清单。

## 安全提示

`mouseshare.exe` 来自本项目源码、由本机交叉编译，非网络下载的陌生程序。
Windows SmartScreen 可能因 exe 未签名而提示——开发期验证可选择「仍要运行」。
正式发布版会做 Authenticode 签名（T-704）。
