# 用户提供的品牌图标

`fesilent-user-icon.png` 是用户于 2026-10-20 上传的原始文件，逐字节保留。

在 macOS 上运行 `python3 scripts/gen_icons.py`，使用系统 Swift/CoreGraphics 生成：

- `icon.png`：1024px，保留黑色底板及白色标志、透明安全边距。
- `icon-32.png`：32px 小图标。
- `tray.png`：44px 透明白色标志，供 macOS 模板菜单栏与 Windows 托盘使用。
- `windows/mouseshare-icon.png`：新应用图标预览。
- `windows/mouseshare.ico`：16/24/32/48/64/128/256px，32 位透明度。

Mac 打包脚本从 `icon.png` 生成 ICNS 并重新签名；Windows 构建由既有 RC 嵌入 ICO。
资源生成不重新设计标志、不拉伸长宽比，也不会再生成旧的蓝紫色图标。
Swift 缓存仅写入项目 `target/icon-tools/`，无需新增应用依赖。
