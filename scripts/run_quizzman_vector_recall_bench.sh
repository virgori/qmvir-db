#!/usr/bin/env bash
# Multi-query recall + ef_search sweep on quizzman (unique Gaussian corpus).
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export PYTHONPATH="/root/qm-linux-bench/scripts:${PYTHONPATH:-}"
export QDRANT_URL="${QDRANT_URL:-http://127.0.0.1:6333}"

ROWS="${ROWS:-10000}"
QUERIES="${QUERIES:-1000}"
DIM="${DIM:-32}"

pip3 install -q --break-system-packages maturin qdrant-client 2>/dev/null || true

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  (cd qm_engine && cargo clean -q)
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
  pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
fi
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'

ensure_qdrant() {
  curl -sf "${QDRANT_URL}/healthz" >/dev/null 2>&1 && return 0
  systemctl start docker 2>/dev/null || true
  docker start qm-qdrant 2>/dev/null || docker run -d --name qm-qdrant -p 6333:6333 qdrant/qdrant:latest
  for _ in $(seq 1 30); do
    curl -sf "${QDRANT_URL}/healthz" >/dev/null && return 0
    sleep 1
  done
  return 1
}

QDRANT_FLAG=()
if ensure_qdrant; then
  QDRANT_FLAG=(--with-qdrant)
  echo "Qdrant enabled at ${QDRANT_URL}"
else
  echo "WARN: Qdrant unavailable — QM-only recall"
fi

echo "=== Sanity (unique self-query) ==="
python3 scripts/vector_ann_sanity.py

echo "=== Multi-query recall @ ef=40 (rows=${ROWS} queries=${QUERIES}) ==="
python3 scripts/vector_recall_bench.py \
  --rows "$ROWS" \
  --dim "$DIM" \
  --queries "$QUERIES" \
  --ef-search 40 \
  --ks 10,50,100 \
  "${QDRANT_FLAG[@]}" \
  --output "/tmp/qm_vector_recall_${ROWS}.json"

echo "=== ef_search sweep ==="
python3 scripts/vector_ef_sweep_bench.py \
  --rows "$ROWS" \
  --dim "$DIM" \
  --queries "$QUERIES" \
  --ef 40,80,120,200,400 \
  --ks 10,50,100 \
  "${QDRANT_FLAG[@]}" \
  --output "/tmp/qm_vector_ef_sweep_${ROWS}.json"

echo "Done. Artifacts:"
echo "  /tmp/qm_vector_recall_${ROWS}.json"
echo "  /tmp/qm_vector_ef_sweep_${ROWS}.json"
