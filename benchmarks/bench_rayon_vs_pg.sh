#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE_DIR="$ROOT_DIR/qm_engine"
BENCH_DIR="$ROOT_DIR/benchmarks"

RUST_CASES="${RUST_CASES:-100000,300000,1000000}"
PY_PROFILE="${PY_PROFILE:-heavy}"
PY_ITERATIONS="${PY_ITERATIONS:-3}"
PG_HOST="${PG_HOST:-127.0.0.1}"
PG_PORT="${PG_PORT:-}"
PG_USER="${PG_USER:-${USER:-postgres}}"
PG_DB="${PG_DB:-postgres}"
PG_UDS_DIR="${PG_UDS_DIR:-}"
QM_HOST="${QM_HOST:-127.0.0.1}"
QM_PORT="${QM_PORT:-55433}"
if [[ -x "/Users/gengyang/Desktop/AI/.venv/bin/python" ]]; then
	BENCH_PYTHON="${BENCH_PYTHON:-/Users/gengyang/Desktop/AI/.venv/bin/python}"
else
	BENCH_PYTHON="${BENCH_PYTHON:-python3}"
fi

if [[ -z "$PG_UDS_DIR" && -S "/tmp/.s.PGSQL.5432" ]]; then
	PG_UDS_DIR="/tmp"
fi

if [[ -z "$PG_PORT" ]]; then
	if command -v pg_isready >/dev/null 2>&1; then
		if pg_isready -h "$PG_HOST" -p 55432 >/dev/null 2>&1; then
			PG_PORT=55432
		elif pg_isready -h "$PG_HOST" -p 5432 >/dev/null 2>&1; then
			PG_PORT=5432
		else
			PG_PORT=55432
		fi
	else
		PG_PORT=55432
	fi
fi

echo "[1/3] Rust parallel hash-join microbench"
pushd "$ENGINE_DIR" >/dev/null
HASH_JOIN_BENCH_CASES="$RUST_CASES" cargo test join_ab_microbench -- --nocapture
popd >/dev/null

echo "[2/3] Python vs Postgres macrobench"
BENCH_ARGS=(
	"$BENCH_DIR/qmvir_vs_postgres_bench.py"
	--profile "$PY_PROFILE"
	--iterations "$PY_ITERATIONS"
	--pg-host "$PG_HOST"
	--pg-port "$PG_PORT"
	--pg-user "$PG_USER"
	--pg-db "$PG_DB"
	--qm-host "$QM_HOST"
	--qm-port "$QM_PORT"
)
if [[ -n "$PG_UDS_DIR" ]]; then
	BENCH_ARGS+=(--pg-uds-dir "$PG_UDS_DIR")
fi
"$BENCH_PYTHON" "${BENCH_ARGS[@]}"

echo "[3/3] Outputs"
echo "- Rust output: terminal logs from cargo test"
echo "- Python JSON: $BENCH_DIR/QMVIR_VS_POSTGRES.json"
echo "- Python Markdown: $BENCH_DIR/QMVIR_VS_POSTGRES.md"
