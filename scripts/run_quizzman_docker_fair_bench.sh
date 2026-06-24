#!/usr/bin/env bash
# Build QM wheel, docker image, run fair all_container benchmarks on quizzman.
#
# QM harness runs inside Docker; Qdrant on same docker bridge network.
# Artifacts: /tmp/qm_qdrant_docker_*.json, /tmp/qm_duckdb_docker_*.json
set -euo pipefail
cd /root/qm-linux-bench
export PIP_BREAK_SYSTEM_PACKAGES=1
export BENCHMARK_HOST="${BENCHMARK_HOST:-quizzman}"

NET=qm_bench_net
QDRANT_NAME=qm-bench-qdrant

mkdir -p docker-wheels

if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[1/4] maturin release wheel..."
  (cd qm_engine && cargo clean -q)
  python3 -m maturin build --release --out /tmp/qm_linux_wheels 2>&1 | tail -5
  WHEEL=$(ls -t /tmp/qm_linux_wheels/qmvir-*.whl /tmp/qm_linux_wheels/qm_engine-*.whl 2>/dev/null | head -1)
  rm -f docker-wheels/*.whl
  cp "$WHEEL" "docker-wheels/$(basename "$WHEEL")"
fi

echo "[2/4] docker build qm-bench..."
docker build -f scripts/docker/Dockerfile.qm-bench -t qm-bench:latest . 2>&1 | tail -8

echo "[3/4] start qdrant on docker network ${NET}..."
docker network create "${NET}" 2>/dev/null || true
docker rm -f "${QDRANT_NAME}" 2>/dev/null || true
# No host port publish — avoids conflict with native qm-qdrant; bench uses docker DNS.
docker run -d --name "${QDRANT_NAME}" --network "${NET}" qdrant/qdrant:latest
for _ in $(seq 1 45); do
  if docker run --rm --network "${NET}" curlimages/curl:8.5.0 -sf "http://${QDRANT_NAME}:6333/healthz" >/dev/null 2>&1; then
    echo "Qdrant ready on ${NET}"
    break
  fi
  sleep 1
done

echo "[4/4] run benchmarks in qm-bench container (fairness_mode=all_container)..."
docker run --rm --network "${NET}" \
  -e QM_DEPLOYMENT=docker_bridge \
  -e QDRANT_DEPLOYMENT=docker_bridge \
  -e DUCKDB_DEPLOYMENT=in_process_embedded \
  -e QDRANT_URL="http://${QDRANT_NAME}:6333" \
  -e BENCHMARK_HOST="${BENCHMARK_HOST}" \
  -v /root/qm-linux-bench/scripts:/bench/scripts:ro \
  -v /tmp:/tmp \
  qm-bench:latest \
  scripts/run_docker_fair_segment_bench.sh 2>&1 | tee /tmp/qm_docker_fair_bench.log

echo "Done. Log: /tmp/qm_docker_fair_bench.log"
