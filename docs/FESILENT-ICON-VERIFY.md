# 用户品牌图标替换验证

日期：2026-10-20；任务：F-1011。

## 变更

- 用户上传原图完整保存在 `crates/app/icons/source/fesilent-user-icon.png`。
- 原图 SHA256：`b7fa3e3f724179565f4f3021cf95dd330efa67a7425c62faa45370d10610b378`，与上传文件逐字节一致。
- 应用 PNG/Windows ICO 保留黑色底板与白色标志；仅调整透明安全边距和平台尺寸，保留长宽比。
- 菜单栏/托盘使用透明白色标志，避免 Mac 模板渲染整个底板。
- 主窗口接入 `icon.png`，供 Windows 标题栏与运行中任务栏使用；Mac Dock 使用包内 ICNS。
- `python3 scripts/gen_icons.py` 调用 macOS 系统 Swift/CoreGraphics 再生成资源，不再重绘旧蓝紫图标；无新增应用依赖。

## 验证结果

- 源图/应用图标/托盘图标目检及独立复核：图形朝向一致，无镜像、翻转；托盘无底板轮廓。
- 应用 PNG：1024×1024 RGBA；小图标32×32；托盘44×44；透明边角及白色模板 alpha 通过。
- ICO：16/24/32/48/64/128/256 七档、32bpp、有效 PNG 帧及连续偏移。七帧均与实际 Windows EXE 中嵌入资源逐字节一致。
- 从 Mac 实际 `.app` 内 ICNS 提取的1024px图标，与应用 PNG 像素完全一致。
- `cargo fmt --check`、`cargo clippy --workspace -- -D warnings`、`cargo test --workspace` 通过，忽略项沿用既有平台限制。
- Mac ARM64/Windows x64 GNU release 重建通过；Windows 链接器原有 deprecated optimization setting 警告不影响产物。
- 两平台包结构/SHA256、Mac ad-hoc签名/安全参数启动检查通过；现有包校验器5项测试通过。

## 本地交付

- `dist/icon-update/fesilent-mouse.exe`
- `dist/icon-update/fesilent-mouse-macOS-arm64.zip`
- `dist/icon-update/SHA256SUMS`

原有 `dist/release/v0.2.0/` 和 GitHub Release 资产保持原样。本次只提供带新图标的本地候选包，未发布新版本。图标缓存及真实 Windows 桌面显示没有作为已实测结果；Windows 验证基于编译、资源与包内容。

## 后续发布（F-1012，2026-10-20）

用户随后要求“更新上传”，已同步版本并重建为v0.2.1，公开新图标预览版。新包位于`dist/release/v0.2.1/`，两平台GitHub下载与SHA均通过。完整发布记录见`FESILENT-RELEASE-v0.2.1-VERIFY.md`；以上F-1011本地验收记录保留。
