#!/bin/bash
# Run on quizzman after source sync: bash /root/qm-linux-bench/scripts/run_linux_search_bench.sh
set -euo pipefail
cd /root/qm-linux-bench
export PYTHONPATH=/root/qm-linux-bench:${PYTHONPATH:-}
exec > /tmp/qm_search_bench.log 2>&1

echo "=== SEARCH BENCH START $(date -Is) ==="
pip3 install --break-system-packages -q numpy 2>/dev/null || pip3 install -q numpy

echo "--- vector_search_audit_benchmark ---"
python3 scripts/vector_search_audit_benchmark.py \
  --iterations 30 \
  --medium-vector-smoke \
  --medium-vector-smoke-size 10000 \
  --medium-vector-smoke-dim 128 \
  --medium-vector-smoke-queries 50 \
  --output /tmp/qm_vector_search_linux.json

echo "--- bridge_materialization (bm25 + hybrid) ---"
python3 scripts/bridge_materialization_audit.py \
  --rows 10000 --cols 8 \
  --bm25-compact --hybrid-compact \
  --output /tmp/qm_bridge_linux.json

echo "--- native_sql json/text/vector ---"
python3 scripts/native_sql_json_text_bench.py \
  --iterations 200 \
  --output /tmp/qm_native_sql_json_text_linux.json

echo "=== SEARCH BENCH DONE $(date -Is) ==="
