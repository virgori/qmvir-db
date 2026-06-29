#!/usr/bin/env bash
# Remove Linux/Windows cross-compile artifacts from a Mac workspace (~2–3 GB).
# Safe to re-run. Does not touch aarch64/x86_64-apple-darwin targets.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="$ROOT/qm_engine/target"

echo "=== Removing cross-compile cargo targets ==="
for d in \
  aarch64-unknown-linux-gnu \
  x86_64-unknown-linux-gnu \
  aarch64-pc-windows-gnullvm \
  x86_64-pc-windows-gnullvm \
  x86_64-pc-windows-gnu \
  x86_64-pc-windows-msvc \
  aarch64-pc-windows-msvc; do
  if [[ -d "$TARGET/$d" ]]; then
    echo "  rm -rf $TARGET/$d"
    rm -rf "$TARGET/$d"
  fi
done

echo "=== Removing local Linux/Windows release binaries ==="
rm -f "$ROOT/build/release"/qm-linux-* "$ROOT/build/release"/qm-windows-* 2>/dev/null || true
rm -f "$ROOT/npm"/qm-linux-* "$ROOT/npm"/qm-win32-* 2>/dev/null || true
rm -f "$ROOT/npm/bin/native"/qm-linux-* "$ROOT/npm/bin/native"/qm-windows-* 2>/dev/null || true

echo "=== Done ==="
du -sh "$TARGET" "$ROOT/build/release" 2>/dev/null || true
