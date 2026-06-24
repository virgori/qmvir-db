#!/usr/bin/env bash
# QM vs PostgreSQL vector INSERT / UPDATE / KNN bench on quizzman.
set -euo pipefail
cd /root/qm-linux-bench
export POSTGRES_DSN="${POSTGRES_DSN:-postgresql://qm_bench:qm_bench@localhost:5432/qm_bench}"
export PIP_BREAK_SYSTEM_PACKAGES=1
sudo -u postgres psql -d qm_bench -c 'CREATE EXTENSION IF NOT EXISTS vector;' 2>/dev/null || true
pip3 install -q --break-system-packages maturin psycopg2-binary 2>/dev/null || true
(cd qm_engine && cargo clean -q)
python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
echo "installing $WHEEL"
pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'

for ROWS in 10000 100000; do
  ITERS=100
  [[ "$ROWS" -eq 100000 ]] && ITERS=50
  OUT="/tmp/qm_pg_vector_${ROWS}.json"
  echo "=== vector bench ${ROWS} rows ==="
  python3 scripts/compare_postgres_vector_bench.py --rows "$ROWS" --iterations "$ITERS" --output "$OUT"
  python3 -c "
import json
d=json.load(open('$OUT'))
print('QM %s/%s @ %s rows' % (d.get('qm_wins'), d.get('total'), d.get('rows')))
for c in d.get('comparison', []):
    if c.get('skipped'):
        continue
    qm=c.get('qm',{})
    pg=c.get('postgresql',{})
    if 'p50_ms' in qm:
        print('  %-32s QM=%.3fms PG=%.3fms %s' % (c['workload'], qm['p50_ms'], pg['p50_ms'], c.get('winner','?')))
    else:
        print('  %-32s QM=%.0f/s PG=%.0f/s %s' % (c['workload'], qm.get('rows_per_sec',0), pg.get('rows_per_sec',0), c.get('winner','?')))
"
done
