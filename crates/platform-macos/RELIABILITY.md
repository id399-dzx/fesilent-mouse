# 平台可靠性回归（2026-10-19）

本轮只修现有后端的退出、恢复与错误上报，不增加平台、系统服务或注入权限。
实现自行编写，没有复制 Deskflow/lan-mouse 代码。跨设备的安全回退顺序仍由 app/core 负责。

## 已覆盖的缺陷

- `PowerMonitor` 刚启动即销毁时，原先可能尚未发布 run loop 指针，`Drop` 未发出停止
  请求就永久 `join`。新增回归在旧实现中 5 秒超时失败；现改用独立取消位和最多 100ms
  的 run loop 等待，初始化完成前后的取消都能被消费。
- `MacCapture` 的停止和析构会解除 grab、恢复鼠标关联和光标，再等待线程结束。run loop
  source/port 由捕获线程移除、失效和释放；创建 source 失败会返回错误。连续禁用熔断时
  立即解除 grab，`check_health()` 可供会话触发安全回退。CG 光标 API 失败返回错误。
- 左右同类修饰键同时按下后，释放一侧会保留另一侧的修饰 flag，避免随后字母、点击
  丢失组合键状态。测试覆盖 Ctrl/Shift/Option/Command 四组。
- Windows 捕获线程先建立消息队列，再发布启动成功，避免紧接着 `stop` 时 `WM_QUIT`
  投递失败而永久等待；安装第二个钩子失败会清理第一个钩子。
- Windows 注入核对 `SendInput` 接受数量。数量不足（包括 UIPI 拦截但错误码为 0）返回
  `PlatformError`，不再把未接受的事件算作成功。释放失败会保留该键/按钮以便重试，
  同一轮仍尝试释放其余项，析构再次尽力释放。

## 自动检查

在项目根目录执行：

```sh
source .tools/env.sh
cargo test -p ms-platform-macos -p ms-platform-windows
cargo clippy -p ms-platform-macos -p ms-platform-windows --all-targets -- -D warnings
cargo check --target x86_64-pc-windows-gnu -p ms-platform-windows
```

电源监听回归只注册/销毁监听，不执行系统睡眠，不读取剪贴板，不安装 event tap，
无需变更辅助功能权限。Windows 注入结果/释放重试逻辑可在 macOS 运行。

## 仍需真机确认

1. 控制远端时停止共享/退出，两端长按键和鼠标按钮释放，本机光标重新可见可移动。
2. macOS 运行中撤销辅助功能，验证 app 的健康检查/权限回退确实接线，重新授权后可重连。
3. 睡眠、唤醒以及启动后立即停止；显示器合盖、拔插、混合缩放后重新确认入屏位置。
4. Windows 权限较高的窗口/UAC 场景应报告限制并恢复本机操作；本轮没有绕过安全桌面。
5. Windows 捕获反复开始/停止，包括刚开始立即停止，不残留钩子或隐藏光标。

`check_health()` 可发现线程退出和 Mac tap 熔断，不能证明 Windows 低级钩子仍存在。
Raw Input 参照看门狗尚未实现；macOS 仍只暴露主屏边界。单元测试和交叉编译通过
不代表真机多屏、睡眠、权限撤销或 72 小时稳定性已通过。
