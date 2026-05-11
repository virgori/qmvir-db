#!/usr/bin/env bash
set -euo pipefail

ROOT="/Users/gengyang/Desktop/AI/QM"
PYTHON="${PYTHON:-python3}"

cd "$ROOT"

echo "[1/3] JOIN + SUM benchmark vs PostgreSQL"
$PYTHON benchmarks/qmvir_vs_postgres_bench.py --stress

echo "[2/3] Chaos scenarios"
$PYTHON benchmarks/qmvir_stress_chaos.py

echo "[3/4] Microbench Vir vs C vs Python"
$PYTHON benchmarks/microbench_runtime_vir_c_py.py --profile quick

echo "[4/4] Done"
echo "- Markdown: benchmarks/QMVIR_VS_POSTGRES.md"
echo "- JSON: benchmarks/QMVIR_VS_POSTGRES.json"
echo "- Chaos: benchmarks/QMVIR_CHAOS_REPORT.json"
echo "- Microbench Markdown: benchmarks/MICROBENCH_PY_VS_C.md"
echo "- Microbench JSON: benchmarks/MICROBENCH_PY_VS_C.json"
