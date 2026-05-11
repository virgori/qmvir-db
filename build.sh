#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")" && pwd)"
LIB_DIR="$ROOT_DIR/lib"
OUT="$ROOT_DIR/qmvir"

OS="$(uname -s)"
ARCH="$(uname -m)"

pick_lib() {
  case "${OS}-${ARCH}" in
    Darwin-arm64) echo "$LIB_DIR/libqm_arm64.a" ;;
    Darwin-x86_64) echo "$LIB_DIR/libqm_x64.a" ;;
    Linux-aarch64) echo "$LIB_DIR/libqm_arm64.a" ;;
    Linux-x86_64) echo "$LIB_DIR/libqm_x64.a" ;;
    *)
      echo "Unsupported platform: ${OS}-${ARCH}" >&2
      exit 1
      ;;
  esac
}

LIB="$(pick_lib)"
if [[ ! -f "$LIB" ]]; then
  echo "Missing static lib: $LIB" >&2
  exit 1
fi

# Example final link step (replace with your real entrypoint/object files).
echo "Using library: $LIB"
echo "Link placeholder complete -> $OUT"
cp "$LIB" "$OUT"
chmod +x "$OUT"
