#!/usr/bin/env bash
# P0 release gate: bulk ingest, vector torture, index fidelity, cross-version.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

PYTHON="${PYTHON:-python3}"
QUICK=0
EXTRA=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --quick) QUICK=1; shift ;;
    --prev-data-dir) export QM_PREV_DATA_DIR="$2"; shift 2 ;;
    --prev-wheel) export QM_PREV_WHEEL="$2"; shift 2 ;;
    *) EXTRA+=("$1"); shift ;;
  esac
done

if ! "$PYTHON" -c "import qm_engine" 2>/dev/null; then
  echo "Building/installing qm_engine..."
  "$PYTHON" -m pip install -e . ${PIP_BREAK_SYSTEM_PACKAGES:+--break-system-packages}
fi

CMD=("$PYTHON" scripts/release_gate_realdata.py)
[[ "$QUICK" -eq 1 ]] && CMD+=(--quick)
[[ -n "${QM_PREV_DATA_DIR:-}" ]] && CMD+=(--prev-data-dir "$QM_PREV_DATA_DIR")
[[ -n "${QM_PREV_WHEEL:-}" ]] && CMD+=(--prev-wheel "$QM_PREV_WHEEL")
CMD+=("${EXTRA[@]}")

echo "Running: ${CMD[*]}"
"${CMD[@]}"
