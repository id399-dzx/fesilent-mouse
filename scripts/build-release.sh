#!/bin/bash
# macOS 构建 Mac 包，并通过 Zig 交叉构建 Windows GNU 版本。
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:-0.2.0}"
OUT="dist/release/v${VERSION}"
mkdir -p "$OUT"
bash scripts/build-macos-app.sh "$VERSION"
ditto -c -k --sequesterRsrc --keepParent dist/fesilent-mouse.app "$OUT/fesilent-mouse-macOS-arm64.zip"
cargo zigbuild --release --target x86_64-pc-windows-gnu -p ms-app --locked
cp target/x86_64-pc-windows-gnu/release/fesilent-mouse.exe "$OUT/fesilent-mouse.exe"
(
  cd "$OUT"
  shasum -a 256 fesilent-mouse-macOS-arm64.zip fesilent-mouse.exe > SHA256SUMS
)
python3 -B scripts/verify-package.py --kind macos --artifact "$OUT/fesilent-mouse-macOS-arm64.zip" --checksums "$OUT/SHA256SUMS" --smoke
python3 -B scripts/verify-package.py --kind windows --artifact "$OUT/fesilent-mouse.exe" --checksums "$OUT/SHA256SUMS"
echo "v${VERSION} 构建资产已生成：${OUT}"
