#!/usr/bin/env bash
# Unified QM publish benchmark runner (Native SQL + search + extras).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PYTHON="${PYTHON:-python3}"
RELEASE_TAG="${RELEASE_TAG:-unreleased}"
QUICK=0
RUNS=""
SKIP_PG=0
INCLUDE_GC=0
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    --runs) RUNS="$2"; shift 2 ;;
    --release-tag) RELEASE_TAG="$2"; shift 2 ;;
    --skip-postgres) SKIP_PG=1; shift ;;
    --include-group-commit) INCLUDE_GC=1; shift ;;
    *) EXTRA_ARGS+=("$1"); shift ;;
  esac
done

echo "== QM Publish Benchmark Suite =="
echo "ROOT=$ROOT"
echo "RELEASE_TAG=$RELEASE_TAG"
echo "POSTGRES_DSN=${POSTGRES_DSN:-<unset>}"

if ! "$PYTHON" -c "import qm_engine" 2>/dev/null; then
  echo "Building/installing qm_engine (pip install -e .)..."
  "$PYTHON" -m pip install -e . ${PIP_BREAK_SYSTEM_PACKAGES:+--break-system-packages}
fi

CMD=("$PYTHON" scripts/publish_benchmark_suite.py --release-tag "$RELEASE_TAG")
[[ "$QUICK" -eq 1 ]] && CMD+=(--quick)
[[ -n "$RUNS" ]] && CMD+=(--runs "$RUNS")
[[ "$SKIP_PG" -eq 1 ]] && CMD+=(--skip-postgres)
[[ "$INCLUDE_GC" -eq 1 ]] && CMD+=(--include-group-commit)
CMD+=("${EXTRA_ARGS[@]}")

echo "Running: ${CMD[*]}"
"${CMD[@]}"

JSON="benchmarks/RELEASE_${RELEASE_TAG//\//_}_linux.json"
MD="benchmarks/RELEASE_${RELEASE_TAG//\//_}_linux.md"
echo ""
echo "Artifacts:"
echo "  $JSON"
echo "  $MD"
