#!/bin/bash
# 构建正式发布资产：macOS Apple Silicon App zip + Windows x86-64 EXE。
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION="${1:-0.1.2}"
OUT="dist/release/v${VERSION}"
APP="${OUT}/MouseShare.app"

rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cargo build --release -p ms-app
cp target/release/mouseshare "$APP/Contents/MacOS/mouseshare"

ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" crates/app/icons/icon.png --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" crates/app/icons/icon.png --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"

sed \
  -e "s/@VERSION@/${VERSION}/g" \
  scripts/release-Info.plist.in > "$APP/Contents/Info.plist"

codesign --force --deep --sign - "$APP"
codesign --verify --deep --strict "$APP"
ditto -c -k --sequesterRsrc --keepParent "$APP" "$OUT/MouseShare-macOS.zip"

cargo zigbuild --release --target x86_64-pc-windows-gnu -p ms-app
cp target/x86_64-pc-windows-gnu/release/mouseshare.exe "$OUT/mouseshare.exe"

(
  cd "$OUT"
  shasum -a 256 MouseShare-macOS.zip mouseshare.exe > SHA256SUMS
)

echo "✓ v${VERSION} 正式资产已生成：${OUT}"
