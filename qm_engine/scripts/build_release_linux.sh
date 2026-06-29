#!/usr/bin/env bash
# Build Linux release binaries (run on Linux host, e.g. quizzman aarch64).
set -euo pipefail
cd "$(dirname "$0")/.."

RELEASE_ARGS=("$@")
if [[ " ${RELEASE_ARGS[*]} " != *" --no-bump "* ]]; then
  export QM_RELEASE_BUMP=1
fi
# shellcheck source=../../scripts/release_common.sh
source "$(dirname "$0")/../../scripts/release_common.sh"

release_clean_stale_names
release_prepare_version
mkdir -p "$QM_OUT"

HOST="$(uname -m)"
echo "=== Linux release build (host=$HOST, v$RELEASE_VERSION) ==="

build_native_aarch64() {
  echo "[1/2] aarch64-unknown-linux-gnu (native) ..."
  cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --bin qm
  release_install_binary target/aarch64-unknown-linux-gnu/release/qm qm-linux-aarch64
  release_verify_binary "$QM_OUT/qm-linux-aarch64" "$RELEASE_VERSION"
}

build_x86_64() {
  echo "[2/2] x86_64-unknown-linux-gnu ..."
  if [[ "$HOST" == "x86_64" || "$HOST" == "amd64" ]]; then
    cargo build --release --target x86_64-unknown-linux-gnu --no-default-features --bin qm
  elif command -v x86_64-linux-gnu-gcc >/dev/null 2>&1; then
    rustup target add x86_64-unknown-linux-gnu 2>/dev/null || true
    cargo build --release --target x86_64-unknown-linux-gnu --no-default-features --bin qm
  elif command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    rustup target add x86_64-unknown-linux-gnu 2>/dev/null || true
    cargo zigbuild --release --target x86_64-unknown-linux-gnu --no-default-features --bin qm
  else
    echo "  ⚠ skipped qm-linux-x86_64 (no cross-linker — run setup_quizzman_build_env.sh)" >&2
    return 0
  fi
  release_install_binary target/x86_64-unknown-linux-gnu/release/qm qm-linux-x86_64
}

if [[ "$HOST" == "aarch64" || "$HOST" == "arm64" ]]; then
  build_native_aarch64
  build_x86_64
elif [[ "$HOST" == "x86_64" || "$HOST" == "amd64" ]]; then
  build_x86_64
  echo "[skip] aarch64 on x86 host (not requested)"
else
  echo "unsupported host arch: $HOST" >&2
  exit 1
fi

release_stamp_and_manifest "$RELEASE_VERSION"
release_verify_all_binaries "$RELEASE_VERSION" || true

echo ""
echo "=== Release binaries (linux, v$RELEASE_VERSION) ==="
ls -lh "$QM_OUT"/qm-linux-* 2>/dev/null || true
echo "Done."
