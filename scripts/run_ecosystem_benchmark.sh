#!/usr/bin/env bash
# Publish-friendly summary: PG + DuckDB + Qdrant segment results.
#
# Set deployment metadata for JSON disclosure (see segment_benchmark_lib.py).
set -euo pipefail
cd "$(dirname "$0")/.."
export PYTHONPATH="scripts:${PYTHONPATH:-}"
export BENCHMARK_HOST="${BENCHMARK_HOST:-$(hostname)}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-native_host}"
export POSTGRESQL_DEPLOYMENT="${POSTGRESQL_DEPLOYMENT:-native_host}"
export DUCKDB_DEPLOYMENT="${DUCKDB_DEPLOYMENT:-in_process_embedded}"
export QDRANT_DEPLOYMENT="${QDRANT_DEPLOYMENT:-docker_bridge}"

ROWS="${ROWS:-100000}"
ITERS="${ITERS:-50}"
OUT_DIR="${OUT_DIR:-/tmp/qm_ecosystem_bench}"
mkdir -p "$OUT_DIR"

echo "=== Ecosystem benchmark (rows=$ROWS) ==="

if [[ -n "${POSTGRES_DSN:-}" ]]; then
  echo "--- PostgreSQL vector ---"
  python3 scripts/compare_postgres_vector_bench.py \
    --rows "$ROWS" --iterations "$ITERS" \
    --output "$OUT_DIR/postgresql_vector.json"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
from segment_benchmark_lib import print_segment_summary
d=json.load(open('$OUT_DIR/postgresql_vector.json'))
d['competitor']='PostgreSQL'
d['competitor_key']='postgresql'
print_segment_summary(d)
"
fi

echo "--- DuckDB OLAP ---"
python3 scripts/compare_duckdb_olap_bench.py \
  --rows "$ROWS" --iterations "${ITERS:-30}" \
  --output "$OUT_DIR/duckdb_olap.json"

echo "--- Qdrant vector ---"
if curl -sf "${QDRANT_URL:-http://127.0.0.1:6333}/healthz" >/dev/null 2>&1; then
  python3 scripts/compare_qdrant_vector_bench.py \
    --rows "$ROWS" --iterations "$ITERS" \
    --output "$OUT_DIR/qdrant_vector.json"
else
  echo "SKIP Qdrant (set QDRANT_URL or start docker qdrant/qdrant)"
fi

python3 scripts/run_segment_benchmark_suite.py \
  --segments duckdb,qdrant \
  --rows "$ROWS" \
  --output-dir "$OUT_DIR"

echo "Artifacts in $OUT_DIR"
