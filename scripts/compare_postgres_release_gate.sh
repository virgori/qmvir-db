#!/usr/bin/env bash
# P0 release gate vs PostgreSQL (requires POSTGRES_DSN + pgvector + pg_trgm).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
PYTHON="${PYTHON:-python3}"
QUICK=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    *) shift ;;
  esac
done
CMD=("$PYTHON" scripts/compare_postgres_release_gate.py)
[[ "$QUICK" -eq 1 ]] && CMD+=(--quick)
echo "POSTGRES_DSN=${POSTGRES_DSN:-<unset>}"
echo "Running: ${CMD[*]}"
"${CMD[@]}"
