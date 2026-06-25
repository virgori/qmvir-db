#!/usr/bin/env bash
# Run beat-PostgreSQL workstreams in parallel on the current host.
#
# Tracks (background jobs):
#   A  strict durable OLTP (per-commit-sync)
#   B  group-commit OLTP
#   C  search @10k + @100k
#   D  vector vs pgvector
#   E  cluster/HA Rust smoke tests
#
# Usage:
#   POSTGRES_DSN=postgresql://user:pass@localhost:5432/db \
#     bash scripts/run_parallel_beat_postgres.sh
#
# Artifacts: benchmarks/parallel_runs/<stamp>/*.json
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

STAMP="$(date -u +%Y%m%d_%H%M%S)"
OUT_DIR="${OUT_DIR:-$ROOT/benchmarks/parallel_runs/$STAMP}"
mkdir -p "$OUT_DIR"
LOG_DIR="$OUT_DIR/logs"
mkdir -p "$LOG_DIR"

export PYTHONPATH="$ROOT/scripts:${PYTHONPATH:-}"
export POSTGRES_DSN="${POSTGRES_DSN:-postgresql://localhost:5432/postgres}"
export QM_DEPLOYMENT="${QM_DEPLOYMENT:-native_host}"
export POSTGRESQL_DEPLOYMENT="${POSTGRESQL_DEPLOYMENT:-native_host}"

echo "=== Parallel beat-PostgreSQL @ $(hostname) ==="
echo "out=$OUT_DIR"
echo "postgres=$POSTGRES_DSN"
python3 --version 2>/dev/null || true
uname -a

need_pg() {
  python3 -c "
import psycopg2, os, sys
try:
    psycopg2.connect(os.environ['POSTGRES_DSN']).close()
except Exception as e:
    print('PostgreSQL unavailable:', e, file=sys.stderr)
    sys.exit(1)
" || {
    echo "Set POSTGRES_DSN to a reachable PostgreSQL instance."
    exit 1
  }
}

track_a() {
  echo "[A] strict OLTP per-commit-sync"
  python3 scripts/compare_postgres_native_sql.py \
    --qm-mode persistent-wal \
    --qm-sync-policy per-commit-sync \
    --output "$OUT_DIR/oltp_strict_per_commit.json" \
    >"$LOG_DIR/track_a.log" 2>&1
}

track_b() {
  echo "[B] group-commit OLTP"
  python3 scripts/compare_postgres_native_sql.py \
    --qm-mode persistent-wal \
    --qm-sync-policy group-commit-sync \
    --output "$OUT_DIR/oltp_group_commit.json" \
    >"$LOG_DIR/track_b.log" 2>&1
}

track_c() {
  echo "[C] search @10k + @100k"
  python3 scripts/compare_postgres_search_bench.py \
    --rows 10000 --iterations 120 \
    --output "$OUT_DIR/search_10k.json" \
    >"$LOG_DIR/track_c_10k.log" 2>&1
  python3 scripts/compare_postgres_search_bench.py \
    --rows 100000 --iterations 40 \
    --output "$OUT_DIR/search_100k.json" \
    >>"$LOG_DIR/track_c_100k.log" 2>&1
}

track_d() {
  echo "[D] vector bench"
  python3 scripts/compare_postgres_vector_bench.py \
    --rows 10000 --dim 32 \
    --output "$OUT_DIR/vector_10k.json" \
    >"$LOG_DIR/track_d.log" 2>&1
}

track_e() {
  echo "[E] cluster/HA smoke tests"
  (
    cd qm_engine
    cargo test --no-default-features cluster::transport cluster::two_phase cluster::replica cluster::shard \
      >"$LOG_DIR/track_e.log" 2>&1
  )
  echo '{"track":"cluster_ha_smoke","status":"ok"}' >"$OUT_DIR/cluster_ha_smoke.json"
}

PIDS=()
NAMES=()

start_track() {
  local name="$1"
  shift
  "$@" &
  PIDS+=("$!")
  NAMES+=("$name")
}

need_pg

start_track "A-oltp-strict" track_a
start_track "B-oltp-group" track_b
start_track "C-search" track_c
start_track "D-vector" track_d
start_track "E-cluster" track_e

FAIL=0
for i in "${!PIDS[@]}"; do
  pid="${PIDS[$i]}"
  name="${NAMES[$i]}"
  if wait "$pid"; then
    echo "✓ $name done (pid $pid)"
  else
    echo "✗ $name FAILED (pid $pid) — see $LOG_DIR/"
    FAIL=1
  fi
done

echo ""
python3 scripts/beat_postgres_scorecard.py --repo-root "$ROOT" "$OUT_DIR" || true

echo ""
echo "Artifacts: $OUT_DIR"
echo "Logs:      $LOG_DIR"
exit "$FAIL"
