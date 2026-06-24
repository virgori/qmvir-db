#!/usr/bin/env bash
# Unique-vector recall audit + ef_search sweep on quizzman (no variance runs).
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export PYTHONPATH="/root/qm-linux-bench/scripts:${PYTHONPATH:-}"
export QDRANT_URL="${QDRANT_URL:-http://127.0.0.1:6333}"

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  (cd qm_engine && cargo clean -q)
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
  pip3 install -q --break-system-packages --force-reinstall "$WHEEL"
fi
python3 -c 'import qm_engine; print("qm_engine:", qm_engine.__file__)'

pip3 install -q --break-system-packages qdrant-client 2>/dev/null || true

echo "=== Sanity (unique self-query) ==="
python3 scripts/vector_ann_sanity.py

ensure_qdrant() {
  curl -sf "${QDRANT_URL}/healthz" >/dev/null
}

if ! ensure_qdrant; then
  echo "WARN: Qdrant down — QM-only audit"
  QDRANT_FLAG=()
else
  QDRANT_FLAG=(--qdrant-url "${QDRANT_URL}")
fi

for ROWS in 10000 100000; do
  QUERIES=1000
  [[ "$ROWS" -eq 100000 ]] && QUERIES=500
  OUT="/tmp/qm_vector_recall_audit_${ROWS}.json"
  echo ""
  echo "=== Recall audit rows=${ROWS} queries=${QUERIES} ==="
  python3 scripts/vector_recall_audit.py \
    --rows "$ROWS" \
    --dim 32 \
    --queries "$QUERIES" \
    --ef-search 40 \
    --ks 10 50 100 \
    --output "$OUT" \
    "${QDRANT_FLAG[@]}"
done

echo ""
echo "=== ef_search sweep @ 10K (200 queries) ==="
python3 scripts/vector_ef_sweep.py \
  --rows 10000 \
  --dim 32 \
  --queries 200 \
  --ef 40 80 120 200 400 \
  --ks 10 50 \
  --output /tmp/qm_vector_ef_sweep_10k.json \
  "${QDRANT_FLAG[@]}"

echo ""
echo "=== ef_search sweep @ 100K (200 queries) ==="
python3 scripts/vector_ef_sweep.py \
  --rows 100000 \
  --dim 32 \
  --queries 200 \
  --ef 40 80 120 200 400 \
  --ks 10 50 \
  --output /tmp/qm_vector_ef_sweep_100k.json \
  "${QDRANT_FLAG[@]}"

echo "Done."
