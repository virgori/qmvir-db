#!/usr/bin/env bash
# Sync QM repo to quizzman and run full competitor benchmarks.
#
# Usage (from Mac / dev machine):
#   bash scripts/sync_and_run_quizzman_bench.sh
#   bash scripts/sync_and_run_quizzman_bench.sh --docker-fair
#   SSH_HOST=mybench bash scripts/sync_and_run_quizzman_bench.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SSH_HOST="${SSH_HOST:-quizzman}"
REMOTE_DIR="${REMOTE_DIR:-/root/qm-linux-bench}"
RUN_DOCKER_FAIR="${RUN_DOCKER_FAIR:-0}"

for arg in "$@"; do
  case "$arg" in
    --docker-fair) RUN_DOCKER_FAIR=1 ;;
    -h|--help)
      sed -n '2,10p' "$0"
      exit 0
      ;;
  esac
done

echo "=== rsync -> ${SSH_HOST}:${REMOTE_DIR} ==="
rsync -az --delete \
  --exclude '.git' \
  --exclude 'target' \
  --exclude 'node_modules' \
  --exclude '_py_legacy' \
  --exclude 'build/release' \
  --exclude '.env' \
  --exclude 'dist' \
  "$ROOT/" "${SSH_HOST}:${REMOTE_DIR}/"

echo "=== SSH benchmark on ${SSH_HOST} ==="
ssh -o BatchMode=yes "${SSH_HOST}" "RUN_DOCKER_FAIR=${RUN_DOCKER_FAIR} bash ${REMOTE_DIR}/scripts/run_quizzman_full_bench.sh"

echo "=== fetch latest JSON artifacts ==="
mkdir -p "$ROOT/benchmarks/quizzman_runs"
rsync -az "${SSH_HOST}:/tmp/qm_*_*.json" "${SSH_HOST}:/tmp/qm_full_bench_*.log" \
  "$ROOT/benchmarks/quizzman_runs/" 2>/dev/null || true
echo "Local copies: $ROOT/benchmarks/quizzman_runs/"
