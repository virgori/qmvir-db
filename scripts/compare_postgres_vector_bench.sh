#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
PYTHON="${PYTHON:-python3}"
QUICK=0
ROWS=""
ITERS=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    --rows) ROWS="$2"; shift 2 ;;
    --iterations) ITERS="$2"; shift 2 ;;
    *) shift ;;
  esac
done
CMD=("$PYTHON" scripts/compare_postgres_vector_bench.py)
[[ "$QUICK" -eq 1 ]] && CMD+=(--rows 10000 --iterations 100)
[[ -n "$ROWS" ]] && CMD+=(--rows "$ROWS")
[[ -n "$ITERS" ]] && CMD+=(--iterations "$ITERS")
echo "POSTGRES_DSN=${POSTGRES_DSN:-<unset>}"
"${CMD[@]}"
