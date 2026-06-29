#!/usr/bin/env bash
# Prepare quizzman (or any Debian/Ubuntu aarch64 host) for QM release builds.
# Run on the remote host after rsync:
#   bash /root/qm-linux-bench/scripts/setup_quizzman_build_env.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

echo "=== setup_quizzman_build_env (host=$(uname -m)) ==="

if command -v apt-get >/dev/null 2>&1; then
  echo "[1/4] apt packages ..."
  export DEBIAN_FRONTEND=noninteractive
  # Debian uses gcc-x86-64-linux-gnu (hyphens), not gcc-x86_64-linux-gnu
  apt-get update -qq
  apt-get install -y -qq \
    build-essential \
    cmake \
    pkg-config \
    python3 \
    python3-pip \
    curl \
    ca-certificates \
    gcc-x86-64-linux-gnu \
    g++-x86-64-linux-gnu \
    binutils-x86-64-linux-gnu \
    crossbuild-essential-amd64 \
    2>/dev/null || apt-get install -y -qq \
    build-essential cmake pkg-config python3 python3-pip curl ca-certificates \
    gcc-x86-64-linux-gnu g++-x86-64-linux-gnu crossbuild-essential-amd64
else
  echo "[1/4] skip apt (not Debian/Ubuntu)"
fi

echo "[2/4] rust targets ..."
if command -v rustup >/dev/null 2>&1; then
  rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu
else
  echo "  rustup not found — using system cargo only"
fi

echo "[3/4] cargo cross-linker config ..."
mkdir -p "$ROOT/qm_engine/.cargo"
cat > "$ROOT/qm_engine/.cargo/config.toml" <<'EOF'
[target.x86_64-unknown-linux-gnu]
linker = "x86_64-linux-gnu-gcc"

[env]
CC_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-gcc"
CXX_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-g++"
AR_x86_64_unknown_linux_gnu = "x86_64-linux-gnu-ar"
CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = "x86_64-linux-gnu-gcc"
EOF
echo "  wrote qm_engine/.cargo/config.toml"

echo "[4/4] optional zig (fallback cross) ..."
if ! command -v zig >/dev/null 2>&1 && command -v apt-get >/dev/null 2>&1; then
  apt-get install -y -qq zig 2>/dev/null || echo "  zig not available via apt — gcc cross-linker is primary"
fi
if command -v cargo-zigbuild >/dev/null 2>&1; then
  echo "  cargo-zigbuild: $(cargo-zigbuild --version)"
elif command -v cargo >/dev/null 2>&1; then
  echo "  installing cargo-zigbuild (optional fallback) ..."
  cargo install cargo-zigbuild --locked 2>/dev/null || echo "  cargo-zigbuild install skipped"
fi

echo ""
echo "=== Toolchain summary ==="
echo "  rustc:  $(rustc --version 2>/dev/null || echo missing)"
echo "  cargo:  $(cargo --version 2>/dev/null || echo missing)"
echo "  gcc-x86: $(command -v x86_64-linux-gnu-gcc 2>/dev/null || echo missing)"
echo "  zig:    $(command -v zig 2>/dev/null || echo missing)"
echo "  version: $(grep '^version' qm_engine/Cargo.toml | head -1)"
rustup target list --installed 2>/dev/null | sed 's/^/  target: /' || true
echo "PASS: setup_quizzman_build_env"
