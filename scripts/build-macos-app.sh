#!/bin/bash
# 编译并打包 Fesilent Mouse，与上游使用独立 bundle id。
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:-0.2.3}"
BIN="target/release/fesilent-mouse"
APP="dist/fesilent-mouse.app"
ICON_SRC="crates/app/icons/icon.png"
# 先确认稳定的签名身份，避免完成编译后才发现指定证书无效。
CODESIGN_IDENTITY="$(python3 -B scripts/select-macos-signing.py)"
cargo build --release -p ms-app --locked
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/fesilent-mouse"
mkdir -p target/macos-iconsets
ICON_TMP="$(mktemp -d target/macos-iconsets/icon.XXXXXX)"
trap 'rm -rf "$ICON_TMP"' EXIT
ICONSET="$ICON_TMP/icon.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
  retina=$((size * 2))
  sips -z "$retina" "$retina" "$ICON_SRC" --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/icon.icns"
sed -e "s/@VERSION@/${VERSION}/g" scripts/release-Info.plist.in > "$APP/Contents/Info.plist"
if [[ "${FESILENT_MACOS_RELEASE:-0}" == "1" ]]; then
  # 先签内部可执行文件、再签外层 App；正式包要求 Hardened Runtime 和安全时间戳。
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP/Contents/MacOS/fesilent-mouse"
  codesign --force --options runtime --timestamp --sign "$CODESIGN_IDENTITY" "$APP"
else
  codesign --force --deep --sign "$CODESIGN_IDENTITY" "$APP"
fi
codesign --verify --deep --strict "$APP"
python3 -B scripts/verify-package.py --kind macos --artifact "$APP" --smoke
if [[ "${FESILENT_MACOS_RELEASE:-0}" == "1" ]]; then
  python3 -B scripts/verify-macos-release.py --app "$APP"
fi
echo "已生成 ${APP}（com.fesilent.mouse，版本 ${VERSION}）"
