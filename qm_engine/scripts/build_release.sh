#!/bin/bash
# macOS release binaries only (local dev / --mac-too on sync script).
# Linux + Windows: build on quizzman via scripts/sync_and_build_release_quizzman.sh
set -euo pipefail
cd "$(dirname "$0")/.."

RELEASE_ARGS=("$@")
# shellcheck source=../../scripts/release_common.sh
source "$(dirname "$0")/../../scripts/release_common.sh"

unset CARGO_TARGET_DIR 2>/dev/null || true
export PYO3_CROSS_PYTHON_VERSION=3.11

release_clean_stale_names
release_prepare_version
mkdir -p "$QM_OUT"

echo "[1/2] aarch64-apple-darwin (Mac ARM) ..."
cargo build --release --target aarch64-apple-darwin --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
release_install_binary target/aarch64-apple-darwin/release/qm qm-macos-arm64
release_verify_binary "$QM_OUT/qm-macos-arm64" "$RELEASE_VERSION"

echo "[2/2] x86_64-apple-darwin (Mac Intel) ..."
cargo build --release --target x86_64-apple-darwin --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
release_install_binary target/x86_64-apple-darwin/release/qm qm-macos-x86_64
release_verify_binary "$QM_OUT/qm-macos-x86_64" "$RELEASE_VERSION"

release_stamp_and_manifest "$RELEASE_VERSION"
release_verify_all_binaries "$RELEASE_VERSION" || true

echo ""
echo "=== macOS release binaries (v$RELEASE_VERSION) ==="
ls -lh "$QM_OUT"/qm-macos-* 2>/dev/null || echo "No binaries found"
echo "Linux/Windows: bash scripts/sync_and_build_release_quizzman.sh"
echo "Done."
