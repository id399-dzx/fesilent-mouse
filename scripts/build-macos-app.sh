#!/bin/bash
# 编译 release 二进制并打包成 mac .app（菜单栏托盘应用，不占 Dock）。
# 编译收在脚本内：曾因「只打包不编译」把陈旧 target/release 二进制原样打进 dist
#（2026-07-05 T-615 交付事故，用户拿到的仍是回退版），故禁止再把编译拆到脚本外。
#
# 本脚本产出的是【测试版】（dist/MouseShare Dev.app，bundle id com.mouseshare.dev），
# 与日常使用的【可用版本】(com.mouseshare.app，置于 ~/Applications/MouseShare-可用版本/)
# 身份隔离，两者在 macOS「辅助功能」里各占一条、互不抢授权（见 OPLOG 2026-06-30）。
set -e
cd "$(dirname "$0")/.."

BIN="target/release/mouseshare"
APP="dist/MouseShare Dev.app"
ICON_SRC="crates/app/icons/icon.png"

cargo build --release -p ms-app

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/mouseshare"

# 从 PNG 生成 icns
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
  sips -z "$s" "$s" "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z "$d" "$d" "$ICON_SRC" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>MouseShare Dev</string>
  <key>CFBundleDisplayName</key><string>MouseShare Dev</string>
  <key>CFBundleExecutable</key><string>mouseshare</string>
  <key>CFBundleIdentifier</key><string>com.mouseshare.dev</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <!-- 暂不设 LSUIElement：accessory 模式下 eframe 设置窗口可能弹不出；
       先用 Regular（有 Dock 图标）保证窗口可靠。纯托盘无 Dock 待 activation 实现。 -->
  <key>NSHighResolutionCapable</key><true/>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
</dict>
</plist>
PLIST

# ad-hoc 签名：让本机能运行（无需开发者证书；首次仍需右键→打开绕过 Gatekeeper）
codesign --force --deep --sign - "$APP" >/dev/null 2>&1 || echo "（codesign 跳过）"

echo "✓ 已生成 $APP（测试版 com.mouseshare.dev；日常版在 ~/Applications/MouseShare-可用版本/）"
