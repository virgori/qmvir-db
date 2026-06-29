#!/usr/bin/env bash
# Build release binaries on quizzman (Linux arm64 host).
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
echo "=== quizzman release build (host=$HOST, v$RELEASE_VERSION) ==="

ensure_zig() {
  if command -v zig >/dev/null 2>&1; then return 0; fi
  echo "Installing zig 0.14.0 ..."
  local zdir="/opt/zig"
  if [[ ! -x "$zdir/zig" ]]; then
    curl -fsSL "https://ziglang.org/download/0.14.0/zig-linux-aarch64-0.14.0.tar.xz" \
      | tar -xJ -C /opt
    mv /opt/zig-linux-aarch64-0.14.0 "$zdir"
  fi
  export PATH="$zdir:$PATH"
  command -v zig >/dev/null 2>&1
}

echo "[1/4] aarch64-unknown-linux-gnu (native) ..."
cargo build --release --target aarch64-unknown-linux-gnu --no-default-features --bin qm
release_install_binary target/aarch64-unknown-linux-gnu/release/qm qm-linux-aarch64
release_verify_binary "$QM_OUT/qm-linux-aarch64" "$RELEASE_VERSION"

echo "[2/4] x86_64-unknown-linux-gnu (gcc cross) ..."
rustup target add x86_64-unknown-linux-gnu 2>/dev/null || true
cargo build --release --target x86_64-unknown-linux-gnu --no-default-features --bin qm
release_install_binary target/x86_64-unknown-linux-gnu/release/qm qm-linux-x86_64

if ensure_zig && command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "[3/4] x86_64-pc-windows-gnullvm ..."
  rustup target add x86_64-pc-windows-gnullvm 2>/dev/null || true
  cargo zigbuild --release --target x86_64-pc-windows-gnullvm --no-default-features --bin qm
  release_install_binary target/x86_64-pc-windows-gnullvm/release/qm.exe qm-windows-x86_64.exe

  echo "[4/4] aarch64-pc-windows-gnullvm ..."
  rustup target add aarch64-pc-windows-gnullvm 2>/dev/null || true
  cargo zigbuild --release --target aarch64-pc-windows-gnullvm --no-default-features --bin qm
  release_install_binary target/aarch64-pc-windows-gnullvm/release/qm.exe qm-windows-aarch64.exe
else
  echo "[3/4] skip Windows (need zig + cargo-zigbuild)"
fi

release_stamp_and_manifest "$RELEASE_VERSION"
release_verify_all_binaries "$RELEASE_VERSION" || true

echo ""
echo "=== Artifacts (v$RELEASE_VERSION) ==="
ls -lh "$QM_OUT"/qm-linux-* "$QM_OUT"/qm-windows-* 2>/dev/null || ls -lh "$QM_OUT"/qm-linux-* 2>/dev/null
echo "Done (macOS: build on Mac only)."
