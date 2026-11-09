#!/bin/bash
# macOS 构建 Mac 包，并通过 Zig 交叉构建 Windows GNU 版本。
set -euo pipefail
cd "$(dirname "$0")/.."
VERSION="${1:-0.2.3}"
MACOS_APP=""
if [[ "$#" -gt 1 ]]; then
  if [[ "$#" -ne 3 || "$2" != "--macos-app" ]]; then
    echo "用法：bash scripts/build-release.sh 版本 [--macos-app 已公证的/fesilent-mouse.app]" >&2
    exit 2
  fi
  MACOS_APP="$3"
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+([.-][A-Za-z0-9.-]+)?$ ]]; then
  echo "版本格式无效。" >&2
  exit 2
fi
case "${FESILENT_MACOS_RELEASE:-0}" in
  0|1) ;;
  *) echo "FESILENT_MACOS_RELEASE 只接受 0 或 1。" >&2; exit 2 ;;
esac
OUT="dist/release/v${VERSION}"
mkdir -p "$OUT"
if [[ -z "$MACOS_APP" ]]; then
  bash scripts/build-macos-app.sh "$VERSION"
  MACOS_APP="dist/fesilent-mouse.app"
else
  if [[ ! -d "$MACOS_APP" || "$(basename "$MACOS_APP")" != "fesilent-mouse.app" ]]; then
    echo "--macos-app 必须指向已经准备好的 fesilent-mouse.app 目录。" >&2
    exit 2
  fi
  python3 -B scripts/verify-package.py --kind macos --artifact "$MACOS_APP" --smoke
fi
if [[ "${FESILENT_MACOS_RELEASE:-0}" == "1" || "$#" -gt 1 ]]; then
  # 只验证，不在构建中隐式上传。保留已公证 App，不能重建后丢失票据。
  if ! python3 -B scripts/verify-macos-release.py --app "$MACOS_APP" --expected-version "$VERSION" --require-notarized; then
    echo "正式分发检查未通过；修复签名/公证后，使用 --macos-app 复用原 App 再打包。公证上传需显式调用 verify-macos-release.py --notarize。" >&2
    exit 1
  fi
fi
ditto -c -k --sequesterRsrc --keepParent "$MACOS_APP" "$OUT/fesilent-mouse-macOS-arm64.zip"
cargo zigbuild --release --target x86_64-pc-windows-gnu -p ms-app --locked
cp target/x86_64-pc-windows-gnu/release/fesilent-mouse.exe "$OUT/fesilent-mouse.exe"
(
  cd "$OUT"
  shasum -a 256 fesilent-mouse-macOS-arm64.zip fesilent-mouse.exe > SHA256SUMS
)
python3 -B scripts/verify-package.py --kind macos --artifact "$OUT/fesilent-mouse-macOS-arm64.zip" --checksums "$OUT/SHA256SUMS" --smoke
python3 -B scripts/verify-package.py --kind windows --artifact "$OUT/fesilent-mouse.exe" --checksums "$OUT/SHA256SUMS"
echo "v${VERSION} 构建资产已生成：${OUT}"
