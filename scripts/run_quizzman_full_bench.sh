#!/usr/bin/env bash
# Full competitor benchmark on quizzman: PostgreSQL + DuckDB + Qdrant (+ optional Docker fair).
#
# Run on quizzman:
#   bash scripts/run_quizzman_full_bench.sh
#
# From Mac (sync + run):
#   bash scripts/sync_and_run_quizzman_bench.sh
#
# Artifacts: /tmp/qm_full_bench_*.log, /tmp/qm_*_*.json
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export PYTHONPATH="/root/qm-linux-bench/scripts:${PYTHONPATH:-}"
export BENCHMARK_HOST="${BENCHMARK_HOST:-quizzman}"
export POSTGRES_DSN="${POSTGRES_DSN:-postgresql://qm_bench:qm_bench@localhost:5432/qm_bench}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-native_host}"
export DUCKDB_DEPLOYMENT="${DUCKDB_DEPLOYMENT:-in_process_embedded}"
export QDRANT_DEPLOYMENT="${QDRANT_DEPLOYMENT:-docker_bridge}"
export POSTGRESQL_DEPLOYMENT="${POSTGRESQL_DEPLOYMENT:-native_host}"

STAMP="$(date -u +%Y%m%d_%H%M%S)"
LOG="/tmp/qm_full_bench_${STAMP}.log"
exec > >(tee -a "$LOG") 2>&1

echo "=== QMvir full benchmark @ ${BENCHMARK_HOST} ==="
echo "log=${LOG}"
echo "qm_engine source: $(pwd)"
python3 --version
uname -a

pip3 install -q --break-system-packages \
  duckdb qdrant-client maturin psycopg2-binary 2>/dev/null || true

sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS vector;' 2>/dev/null || true
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;' 2>/dev/null || true

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[build] maturin release wheel..."
  [[ "${CARGO_CLEAN:-0}" == "1" ]] && (cd qm_engine && cargo clean -q)
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -8
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qm_engine-*.whl /tmp/qm_linux_wheels/qmvir-*.whl 2>/dev/null | head -1)
  echo "installing ${WHEEL}"
  pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
fi
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'

summarize_json() {
  local path="$1" label="$2"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
try:
    from segment_benchmark_lib import print_segment_summary
    d=json.load(open('$path'))
    print('--- $label ---')
    if 'comparison' in d:
        print_segment_summary(d)
    else:
        print('QM %s/%s' % (d.get('qm_wins','?'), d.get('total','?')))
        for c in d.get('comparison', []):
            if c.get('skipped'): continue
            qm=c.get('qm',{}); pg=c.get('postgresql',{}); comp=c.get('competitor',{})
            if 'p50_ms' in qm:
                other=pg or comp
                print('  %-35s QM=%.3f other=%.3f %s' % (c['workload'], qm['p50_ms'], other.get('p50_ms',0), c.get('winner','?')))
            elif 'rows_per_sec' in qm:
                other=pg or comp
                print('  %-35s QM=%.0f/s other=%.0f/s %s' % (c['workload'], qm['rows_per_sec'], other.get('rows_per_sec',0), c.get('winner','?')))
except Exception as e:
    print('summary failed for $path:', e)
"
}

echo ""
echo "========== PostgreSQL OLTP (native SQL) =========="
OLT_OUT="/tmp/qm_pg_oltp_${STAMP}.json"
if python3 scripts/compare_postgres_native_sql.py \
  --qm-mode persistent-wal \
  --qm-sync-policy group-commit-sync \
  --iterations 500 \
  --output "$OLT_OUT" 2>&1; then
  summarize_json "$OLT_OUT" "PostgreSQL OLTP"
else
  echo "WARN: OLTP bench failed (see log)"
fi

echo ""
echo "========== PostgreSQL Vector =========="
export SKIP_BUILD=1
bash scripts/run_quizzman_vector_bench.sh

echo ""
echo "========== PostgreSQL Search =========="
bash scripts/run_quizzman_search_bench.sh

echo ""
echo "========== DuckDB OLAP + Qdrant Vector =========="
bash scripts/run_quizzman_segment_bench.sh

if [[ "${RUN_DOCKER_FAIR:-0}" == "1" ]]; then
  echo ""
  echo "========== Docker fair (all_container) =========="
  bash scripts/run_quizzman_docker_fair_bench.sh
fi

echo ""
echo "========== Aggregate scorecard =========="
python3 - <<'PY'
import json, glob, os
from pathlib import Path

def wins(path):
    try:
        d = json.loads(Path(path).read_text())
    except Exception:
        return None
    return {
        "file": path,
        "segment": d.get("segment") or d.get("label") or Path(path).stem,
        "rows": d.get("rows"),
        "qm_wins": d.get("qm_wins"),
        "total": d.get("total"),
        "competitor": d.get("competitor"),
        "publish_ready": d.get("publish_ready"),
    }

patterns = [
    "/tmp/qm_pg_vector_*.json",
    "/tmp/qm_search_542_*.json",
    "/tmp/qm_duckdb_olap_*.json",
    "/tmp/qm_qdrant_vector_*.json",
    "/tmp/qm_pg_oltp_*.json",
    "/tmp/qm_qdrant_docker_*.json",
    "/tmp/qm_duckdb_docker_*.json",
]
seen = set()
print(f"{'artifact':<40} {'segment':<20} {'QM wins':<12} {'rows':<10}")
print("-" * 85)
for pat in patterns:
    for p in sorted(glob.glob(pat), key=os.path.getmtime, reverse=True):
        if p in seen:
            continue
        seen.add(p)
        w = wins(p)
        if not w or w.get("qm_wins") is None:
            continue
        print(f"{Path(p).name:<40} {str(w.get('segment','')):<20} {w['qm_wins']}/{w['total']:<9} {w.get('rows','')}")
PY

echo ""
echo "Full benchmark done. Log: ${LOG}"
