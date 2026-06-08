#!/bin/bash
set -e
cd "$(dirname "$0")/.."

export PYO3_CROSS_PYTHON_VERSION=3.11
OUT=../build/release
mkdir -p "$OUT"

# 1. Mac ARM (native)
echo "[1/6] aarch64-apple-darwin (Mac ARM) ..."
cargo build --release --target aarch64-apple-darwin --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
cp target/aarch64-apple-darwin/release/qm "$OUT/qm-macos-arm64"
echo "  → $OUT/qm-macos-arm64 ($(du -h "$OUT/qm-macos-arm64" | cut -f1))"

# 2. Mac Intel
echo "[2/6] x86_64-apple-darwin (Mac Intel) ..."
cargo build --release --target x86_64-apple-darwin --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
cp target/x86_64-apple-darwin/release/qm "$OUT/qm-macos-x86_64"
echo "  → $OUT/qm-macos-x86_64 ($(du -h "$OUT/qm-macos-x86_64" | cut -f1))"

# 3. Linux x86_64
echo "[3/6] x86_64-unknown-linux-gnu (Linux x86) ..."
cargo zigbuild --release --target x86_64-unknown-linux-gnu --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
cp target/x86_64-unknown-linux-gnu/release/qm "$OUT/qm-linux-x86_64"
echo "  → $OUT/qm-linux-x86_64 ($(du -h "$OUT/qm-linux-x86_64" | cut -f1))"

# 4. Linux ARM64
echo "[4/6] aarch64-unknown-linux-gnu (Linux ARM) ..."
cargo zigbuild --release --target aarch64-unknown-linux-gnu --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
cp target/aarch64-unknown-linux-gnu/release/qm "$OUT/qm-linux-aarch64"
echo "  → $OUT/qm-linux-aarch64 ($(du -h "$OUT/qm-linux-aarch64" | cut -f1))"

# 5. Windows x86_64 (gnullvm — no python lib needed at link time)
echo "[5/6] x86_64-pc-windows-gnullvm (Windows x86) ..."
cargo zigbuild --release --target x86_64-pc-windows-gnullvm --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
if [ -f target/x86_64-pc-windows-gnullvm/release/qm.exe ]; then
    cp target/x86_64-pc-windows-gnullvm/release/qm.exe "$OUT/qm-windows-x86_64.exe"
    echo "  → $OUT/qm-windows-x86_64.exe ($(du -h "$OUT/qm-windows-x86_64.exe" | cut -f1))"
else
    echo "  ⚠ Windows x86_64 build failed, trying gnu target..."
    cargo zigbuild --release --target x86_64-pc-windows-gnu --no-default-features --bin qm 2>&1 | grep -E "Compiling|Finished|error"
    if [ -f target/x86_64-pc-windows-gnu/release/qm.exe ]; then
        cp target/x86_64-pc-windows-gnu/release/qm.exe "$OUT/qm-windows-x86_64.exe"
        echo "  → $OUT/qm-windows-x86_64.exe ($(du -h "$OUT/qm-windows-x86_64.exe" | cut -f1))"
    else
        echo "  ✗ Windows x86_64 failed"
    fi
fi

# 6. Windows ARM64
echo "[6/6] aarch64-pc-windows-gnullvm (Windows ARM) ..."
cargo zigbuild --release --target aarch64-pc-windows-gnullvm --no-default-features --bin qm 2>&1 | grep -E "Compiling qm_engine|Finished|error"
if [ -f target/aarch64-pc-windows-gnullvm/release/qm.exe ]; then
    cp target/aarch64-pc-windows-gnullvm/release/qm.exe "$OUT/qm-windows-aarch64.exe"
    echo "  → $OUT/qm-windows-aarch64.exe ($(du -h "$OUT/qm-windows-aarch64.exe" | cut -f1))"
else
    echo "  ✗ Windows ARM64 failed"
fi

echo ""
echo "=== Release binaries ==="
ls -lh "$OUT"/qm-* 2>/dev/null || echo "No binaries found"
echo ""
echo "Done."
