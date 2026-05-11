#!/bin/bash
# Build script for QM Native extensions
# Usage: ./build_native.sh [--release|--dev]

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
QM_ROOT="$(dirname "$SCRIPT_DIR")"
NATIVE_DIR="$QM_ROOT/qm_native"

cd "$NATIVE_DIR"

# Check for Rust
if ! command -v cargo &> /dev/null; then
    echo "Error: Rust/Cargo not found. Install from https://rustup.rs/"
    exit 1
fi

# Check for maturin
if ! command -v maturin &> /dev/null; then
    echo "Installing maturin..."
    pip install maturin
fi

# Determine build mode
MODE="${1:---release}"

echo "Building QM Native extensions ($MODE)..."
echo "Working directory: $NATIVE_DIR"

if [[ "$MODE" == "--dev" ]]; then
    maturin develop
else
    maturin develop --release
fi

echo ""
echo "Build complete! Testing import..."
python -c "
from qm_native import NATIVE_AVAILABLE, HNSWIndex
print(f'Native available: {NATIVE_AVAILABLE}')
if NATIVE_AVAILABLE:
    import numpy as np
    index = HNSWIndex(dim=128)
    print(f'Created HNSW index: dim=128')
    print('✓ All tests passed!')
"
