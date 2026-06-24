#!/usr/bin/env bash
# Run on quizzman after sync: bash /root/qm-linux-bench/scripts/run_quizzman_search_bench.sh
set -euo pipefail
cd /root/qm-linux-bench
export POSTGRES_DSN="${POSTGRES_DSN:-postgresql://qm_bench:qm_bench@localhost:5432/qm_bench}"
export PIP_BREAK_SYSTEM_PACKAGES=1
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS vector;' 2>/dev/null || true
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS pg_trgm;' 2>/dev/null || true
pip3 install -q --break-system-packages maturin psycopg2-binary 2>/dev/null || true
(cd qm_engine && cargo clean -q)
echo '[build] maturin release...'
python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl | head -1)
echo "installing $WHEEL"
pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'
echo '=== 10K search bench ==='
python3 scripts/compare_postgres_search_bench.py --rows 10000 --iterations 100 --output /tmp/qm_search_542_10k.json
echo '=== 100K search bench ==='
python3 scripts/compare_postgres_search_bench.py --rows 100000 --iterations 50 --output /tmp/qm_search_542_100k.json
python3 -c '
import json
for path, label in [("/tmp/qm_search_542_10k.json","10K"),("/tmp/qm_search_542_100k.json","100K")]:
    d=json.load(open(path))
    print("\n=== %s: QM %s/%s ===" % (label, d["qm_wins"], d["total"]))
    for c in d["comparison"]:
        if c.get("skipped"):
            continue
        print("  %-35s QM=%.3f PG=%.3f %s" % (c["workload"], c["qm"]["p50_ms"], c["postgresql"]["p50_ms"], c["winner"]))
'
