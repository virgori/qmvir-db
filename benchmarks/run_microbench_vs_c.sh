#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-quick}"
PYTHON="${PYTHON:-python3}"

cd "$ROOT_DIR"

$PYTHON benchmarks/microbench_runtime_vir_c_py.py --profile "$PROFILE"

echo "Outputs:"
echo "- JSON: $ROOT_DIR/benchmarks/MICROBENCH_PY_VS_C.json"
echo "- MD:   $ROOT_DIR/benchmarks/MICROBENCH_PY_VS_C.md (Vir vs C vs Python)"
