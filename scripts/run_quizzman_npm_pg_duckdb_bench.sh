#!/usr/bin/env bash
# quizzman: install npm qmvir + fair native benchmarks vs PostgreSQL and DuckDB.
#
# QM Python bench uses maturin wheel (NativeSqlEngine); npm installs official CLI
# binary (postinstall → GitHub v6.1.1 linux-aarch64) for release-path verification.
#
# Fairness: all_native_host (PG systemd) / qm_native_vs_embedded_competitor (DuckDB :memory:)
#
# Usage on quizzman:
#   bash scripts/run_quizzman_npm_pg_duckdb_bench.sh
# From Mac:
#   rsync ... && ssh quizzman 'bash /root/qm-linux-bench/scripts/run_quizzman_npm_pg_duckdb_bench.sh'
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export PYTHONPATH="/root/qm-linux-bench/scripts:${PYTHONPATH:-}"
export BENCHMARK_HOST="${BENCHMARK_HOST:-quizzman}"
export POSTGRES_DSN="${POSTGRES_DSN:-postgresql://qm_bench:qm_bench@localhost:5432/qm_bench}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-native_host}"
export POSTGRESQL_DEPLOYMENT="${POSTGRESQL_DEPLOYMENT:-native_host}"
export DUCKDB_DEPLOYMENT="${DUCKDB_DEPLOYMENT:-in_process_embedded}"

STAMP="$(date -u +%Y%m%d_%H%M%S)"
LOG="/tmp/qm_npm_pg_duckdb_bench_${STAMP}.log"
exec > >(tee -a "$LOG") 2>&1

echo "=== npm + PostgreSQL + DuckDB fair bench @ ${BENCHMARK_HOST} ==="
echo "log=${LOG}"
uname -a
python3 --version

echo ""
echo "========== [1] Install Node.js + npm =========="
if ! command -v npm >/dev/null 2>&1; then
  apt-get update -qq
  apt-get install -y -qq nodejs npm ca-certificates curl
fi
echo "node: $(node --version 2>/dev/null || echo missing)"
echo "npm:  $(npm --version 2>/dev/null || echo missing)"

QM_NPM_VERSION="$(python3 -c "import json; print(json.load(open('npm/package.json'))['version'])")"
echo ""
echo "========== [2] npm install -g qmvir@${QM_NPM_VERSION} (local package → GitHub binaries) =========="
# Drop stale bundled binaries so postinstall fetches GitHub release for this version.
rm -rf /root/qm-linux-bench/npm/bin/native/*
npm install -g "/root/qm-linux-bench/npm" 2>&1 | tail -15
echo "qm CLI: $(command -v qm 2>/dev/null || command -v qmvir 2>/dev/null || echo missing)"
qm --version 2>&1 || qmvir --version 2>&1 || true

echo ""
echo "========== [3] Python qm_engine wheel (bench harness) =========="
pip3 install -q --break-system-packages duckdb maturin psycopg2-binary 2>/dev/null || true
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS vector;' 2>/dev/null || true
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;' 2>/dev/null || true

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -6
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qm_engine-*.whl /tmp/qm_linux_wheels/qmvir-*.whl 2>/dev/null | head -1)
  echo "installing ${WHEEL}"
  pip3 uninstall -y qmvir qm_engine qm-engine 2>/dev/null || true
  pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
fi
python3 -c '
import importlib.metadata as m
import qm_engine
for name in ("qm_engine", "qmvir", "qm-engine"):
    try:
        print("qm_engine:", m.version(name), qm_engine.__file__)
        break
    except m.PackageNotFoundError:
        continue
else:
    print("qm_engine:", qm_engine.__file__)
'

summarize_json() {
  local path="$1" label="$2"
  python3 -c "
import json, sys
sys.path.insert(0, 'scripts')
try:
    from segment_benchmark_lib import print_segment_summary
    d=json.load(open('$path'))
    print('--- $label ---')
    dep=d.get('deployment',{})
    print('fairness_mode:', dep.get('fairness_mode','?'), '| publish_ready:', dep.get('publish_ready'))
    if 'comparison' in d:
        print_segment_summary(d)
    else:
        print('QM %s/%s' % (d.get('qm_wins','?'), d.get('total','?')))
except Exception as e:
    print('summary failed for $path:', e)
"
}

echo ""
echo "========== [4] PostgreSQL OLTP (fair native) =========="
OLT_OUT="/tmp/qm_pg_oltp_${STAMP}.json"
python3 scripts/compare_postgres_native_sql.py \
  --qm-mode persistent-wal \
  --qm-sync-policy group-commit-sync \
  --iterations 500 \
  --output "$OLT_OUT"
summarize_json "$OLT_OUT" "PostgreSQL OLTP"

echo ""
echo "========== [5] PostgreSQL Vector @10k + @100k =========="
export SKIP_BUILD=1
bash scripts/run_quizzman_vector_bench.sh

echo ""
echo "========== [6] DuckDB OLAP (fair embedded) =========="
for ROWS in 100000 1000000; do
  ITERS=30
  [[ "$ROWS" -eq 1000000 ]] && ITERS=15
  OUT="/tmp/qm_duckdb_olap_${ROWS}.json"
  echo "--- DuckDB rows=${ROWS} ---"
  python3 scripts/compare_duckdb_olap_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"
  summarize_json "$OUT" "DuckDB OLAP @${ROWS}"
done

echo ""
echo "========== Scorecard =========="
python3 - <<'PY'
import json, glob, os
from pathlib import Path

def row(path):
    try:
        d = json.loads(Path(path).read_text())
    except Exception:
        return None
    dep = d.get("deployment", {})
    return (
        Path(path).name,
        d.get("competitor") or "PostgreSQL",
        dep.get("fairness_mode", "?"),
        f"{d.get('qm_wins','?')}/{d.get('total','?')}",
        d.get("rows", ""),
    )

patterns = [
    f"/tmp/qm_pg_oltp_*{os.environ.get('STAMP','')}*.json",
    "/tmp/qm_pg_vector_*.json",
    "/tmp/qm_duckdb_olap_*.json",
]
# fallback stamp from env not exported to python — scan recent
patterns = [
    "/tmp/qm_pg_oltp_*.json",
    "/tmp/qm_pg_vector_*.json",
    "/tmp/qm_duckdb_olap_*.json",
]
seen = set()
print(f"{'file':<38} {'vs':<12} {'fairness':<28} {'QM wins':<10} rows")
print("-" * 95)
for pat in patterns:
    for p in sorted(glob.glob(pat), key=os.path.getmtime, reverse=True):
        if p in seen:
            continue
        seen.add(p)
        r = row(p)
        if not r:
            continue
        print(f"{r[0]:<38} {r[1]:<12} {r[2]:<28} {r[3]:<10} {r[4]}")
        if len(seen) >= 8:
            break
PY

echo ""
echo "Done. Log: ${LOG}"
