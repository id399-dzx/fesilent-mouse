# 更新日志

## v0.1.2 — 2026-07-12

- 文件传输启动及接收前自动清理超过 8 小时的 `.ms-part` 残留文件。
- Windows EXE 加入 MouseShare 专属蓝紫图标，覆盖 16–256px 多尺寸资源。
- Windows 控制 Mac 的纵向滚轮速度范围扩展为 1.0×–3.0×，支持即时生效和持久化。
- 增加剪贴板历史、传输实时速率、剩余时间、最近传输记录和诊断导出。
- 修复多文件粘贴仅保留最后一个文件、macOS Finder 文件引用解析、屏幕边缘越界等问题。
- 保持 QUIC + TLS、设备指纹固定和 6 位 SAS 配对安全机制。

### 发布限制

- macOS 包为 ad-hoc 签名，未做 Apple 公证；首次运行需右键“打开”并授予辅助功能权限。
- Windows EXE 未做 Authenticode 签名，SmartScreen 可能提示风险。
- 本次仅发布 Apple Silicon macOS 版本和 x86-64 Windows 版本。
