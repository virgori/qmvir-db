#!/usr/bin/env bash
# Run inside qm-bench Docker container (QM_DEPLOYMENT=docker_bridge).
set -euo pipefail
cd /bench
export PYTHONPATH="/bench/scripts:${PYTHONPATH:-}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-docker_bridge}"
export QDRANT_DEPLOYMENT="${QDRANT_DEPLOYMENT:-docker_bridge}"
export QDRANT_URL="${QDRANT_URL:-http://qdrant:6333}"

python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'
echo "fairness: QM=${QM_DEPLOYMENT} Qdrant=${QDRANT_DEPLOYMENT} url=${QDRANT_URL}"

for ROWS in 10000 100000; do
  ITERS=50
  [[ "$ROWS" -eq 100000 ]] && ITERS=30
  OUT="/tmp/qm_qdrant_docker_${ROWS}.json"
  echo "--- Qdrant vector rows=${ROWS} (all_container) ---"
  python3 scripts/compare_qdrant_vector_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
from segment_benchmark_lib import print_segment_summary
d=json.load(open('$OUT'))
print_segment_summary(d)
"
done

echo '=== DuckDB OLAP (QM in container, DuckDB embedded) ==='
for ROWS in 100000 1000000; do
  ITERS=30
  [[ "$ROWS" -eq 1000000 ]] && ITERS=15
  OUT="/tmp/qm_duckdb_docker_${ROWS}.json"
  python3 scripts/compare_duckdb_olap_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
from segment_benchmark_lib import print_segment_summary
d=json.load(open('$OUT'))
print_segment_summary(d)
"
done
