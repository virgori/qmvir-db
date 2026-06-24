#!/usr/bin/env bash
# QM vs DuckDB (OLAP) + Qdrant (vector) on quizzman.
#
# Deployment on quizzman (default):
#   QM         -> native_host (maturin wheel, in-process)
#   PostgreSQL -> native_host (apt systemd, when using PG benches)
#   DuckDB     -> in_process_embedded (Python duckdb :memory:)
#   Qdrant     -> docker_bridge (qm-qdrant container)
#
# Override: QM_DEPLOYMENT, DUCKDB_DEPLOYMENT, QDRANT_DEPLOYMENT, POSTGRESQL_DEPLOYMENT
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export PYTHONPATH="/root/qm-linux-bench/scripts:${PYTHONPATH:-}"
export BENCHMARK_HOST="${BENCHMARK_HOST:-quizzman}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-native_host}"
export DUCKDB_DEPLOYMENT="${DUCKDB_DEPLOYMENT:-in_process_embedded}"
export QDRANT_DEPLOYMENT="${QDRANT_DEPLOYMENT:-docker_bridge}"
export POSTGRESQL_DEPLOYMENT="${POSTGRESQL_DEPLOYMENT:-native_host}"

pip3 install -q --break-system-packages duckdb qdrant-client maturin 2>/dev/null || true

ensure_qdrant() {
  if curl -sf "${QDRANT_URL:-http://127.0.0.1:6333}/healthz" >/dev/null 2>&1; then
    echo "Qdrant already up at ${QDRANT_URL:-http://127.0.0.1:6333}"
    return 0
  fi
  if command -v docker >/dev/null 2>&1; then
    systemctl start docker 2>/dev/null || service docker start 2>/dev/null || true
    docker rm -f qm-qdrant 2>/dev/null || true
    docker run -d --name qm-qdrant -p 6333:6333 qdrant/qdrant:latest
    for _ in $(seq 1 45); do
      if curl -sf http://127.0.0.1:6333/healthz >/dev/null 2>&1; then
        echo "Qdrant docker ready"
        export QDRANT_URL=http://127.0.0.1:6333
        return 0
      fi
      sleep 1
    done
  fi
  echo "WARN: Qdrant not available — skip vector vs Qdrant bench"
  return 1
}

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  (cd qm_engine && cargo clean -q)
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
  echo "installing $WHEEL"
  pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
fi
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'
echo "benchmark deployment: QM=${QM_DEPLOYMENT} DuckDB=${DUCKDB_DEPLOYMENT} Qdrant=${QDRANT_DEPLOYMENT}"

echo '=== DuckDB OLAP ==='
for ROWS in 100000 1000000; do
  ITERS=30
  [[ "$ROWS" -eq 1000000 ]] && ITERS=15
  OUT="/tmp/qm_duckdb_olap_${ROWS}.json"
  echo "--- rows=${ROWS} ---"
  python3 scripts/compare_duckdb_olap_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
from segment_benchmark_lib import print_segment_summary
d=json.load(open('$OUT'))
print_segment_summary(d)
"
done

echo '=== Qdrant Vector ==='
if ensure_qdrant; then
  for ROWS in 10000 100000; do
    ITERS=50
    [[ "$ROWS" -eq 100000 ]] && ITERS=30
    OUT="/tmp/qm_qdrant_vector_${ROWS}.json"
    echo "--- rows=${ROWS} ---"
    if python3 scripts/compare_qdrant_vector_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"; then
      python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
from segment_benchmark_lib import print_segment_summary
d=json.load(open('$OUT'))
print_segment_summary(d)
"
    else
      echo "Qdrant bench failed for rows=${ROWS}"
    fi
  done
else
  echo "Skipped Qdrant benchmarks"
fi

echo '=== Segment suite summary ==='
python3 scripts/run_segment_benchmark_suite.py \
  --segments duckdb,qdrant \
  --rows 100000 \
  --iterations 30 \
  --output-dir /tmp/qm_segments
