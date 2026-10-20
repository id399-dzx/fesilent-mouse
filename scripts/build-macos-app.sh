#!/bin/bash
# 编译并打包 Fesilent Mouse，与上游使用独立 bundle id。
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:-0.2.1}"
BIN="target/release/fesilent-mouse"
APP="dist/fesilent-mouse.app"
ICON_SRC="crates/app/icons/icon.png"
cargo build --release -p ms-app --locked
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/fesilent-mouse"
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"
sed -e "s/@VERSION@/${VERSION}/g" scripts/release-Info.plist.in > "$APP/Contents/Info.plist"
codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
python3 -B scripts/verify-package.py --kind macos --artifact "$APP" --smoke
echo "已生成 ${APP}（com.fesilent.mouse，版本 ${VERSION}）"
