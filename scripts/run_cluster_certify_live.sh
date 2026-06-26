#!/usr/bin/env bash
# Start two qm processes (HA transport + TLS), run `qm cluster certify`, tear down.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QM_ENGINE="$ROOT/qm_engine"
QM_BIN="${QM_BIN:-}"
TRANSPORT_A_PORT="${TRANSPORT_A_PORT:-55441}"
TRANSPORT_B_PORT="${TRANSPORT_B_PORT:-55442}"
PG_PORT_A="${PG_PORT_A:-55433}"
PG_PORT_B="${PG_PORT_B:-55434}"
TLS_DIR="${TLS_DIR:-$ROOT/.cluster-tls-dev}"
WORKDIR="${WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/qm-cluster-certify.XXXXXX")}"
PIDS=()

cleanup() {
  for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  if [[ "${KEEP_WORKDIR:-0}" != "1" ]]; then
    rm -rf "$WORKDIR"
  fi
}
trap cleanup EXIT INT TERM

port_open() {
  if command -v nc >/dev/null 2>&1; then
    nc -z 127.0.0.1 "$1" 2>/dev/null
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/"$1") >/dev/null 2>&1
}

wait_for_port() {
  local port=$1
  local label=$2
  for _ in $(seq 1 60); do
    if port_open "$port"; then
      return 0
    fi
    sleep 0.25
  done
  echo "ERROR: $label port $port did not open" >&2
  return 1
}

resolve_qm_bin() {
  local target_dir
  if command -v python3 >/dev/null 2>&1; then
    target_dir="$(
      cargo metadata --manifest-path "$QM_ENGINE/Cargo.toml" --format-version 1 \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['target_directory'])"
    )"
  else
    target_dir="$(
      cargo metadata --manifest-path "$QM_ENGINE/Cargo.toml" --format-version 1 \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1
    )"
  fi
  if [[ -x "${QM_BIN:-}" ]]; then
    return 0
  fi
  if [[ -x "$target_dir/debug/qm" ]]; then
    QM_BIN="$target_dir/debug/qm"
  elif [[ -x "$target_dir/release/qm" ]]; then
    QM_BIN="$target_dir/release/qm"
  elif [[ -x "$QM_ENGINE/target/debug/qm" ]]; then
    QM_BIN="$QM_ENGINE/target/debug/qm"
  elif [[ -x "$QM_ENGINE/target/release/qm" ]]; then
    QM_BIN="$QM_ENGINE/target/release/qm"
  else
    echo "ERROR: qm binary not found under $target_dir or $QM_ENGINE/target" >&2
    exit 1
  fi
}

echo "=== Build qm (no python) ==="
cargo build --manifest-path "$QM_ENGINE/Cargo.toml" --no-default-features --bin qm -q
resolve_qm_bin
echo "  using: $QM_BIN"

echo "=== Dev TLS certs ==="
bash "$ROOT/scripts/generate_cluster_tls_dev.sh" "$TLS_DIR" >/dev/null
TLS_CERT="$TLS_DIR/cluster-dev.crt"
TLS_KEY="$TLS_DIR/cluster-dev.key"

TRANSPORT_A="127.0.0.1:${TRANSPORT_A_PORT}"
TRANSPORT_B="127.0.0.1:${TRANSPORT_B_PORT}"
DATA_A="$WORKDIR/node-a"
DATA_B="$WORKDIR/node-b"
mkdir -p "$DATA_A" "$DATA_B"

cluster_common_env() {
  export QM_CLUSTER_ENABLE=1
  export QM_CLUSTER_SHARD_ENDPOINTS="0=${TRANSPORT_A},1=${TRANSPORT_A},2=${TRANSPORT_B},3=${TRANSPORT_B}"
  export QM_CLUSTER_SHARD_REPLICAS="0=${TRANSPORT_B},2=${TRANSPORT_B}"
  export QM_CLUSTER_WAL_REPLICATE=1
  export QM_CLUSTER_WAL_SYNC=1
  export QM_CLUSTER_WAL_PEERS="${TRANSPORT_B}"
  export QM_CLUSTER_FAILOVER=1
  export QM_CLUSTER_FENCING=1
  export QM_CLUSTER_2PC=1
  export QM_CLUSTER_META_PEERS="${TRANSPORT_B}"
  export QM_CLUSTER_TLS_CERT="$TLS_CERT"
  export QM_CLUSTER_TLS_KEY="$TLS_KEY"
  export QM_CLUSTER_TLS_CA="$TLS_CERT"
  export QM_ADMIN_PASSWORD=certify-test
}

echo "=== Start standby (node B) transport :${TRANSPORT_B_PORT} ==="
(
  cluster_common_env
  export QM_CLUSTER_NODE_ID=2
  export QM_CLUSTER_TRANSPORT_PORT="$TRANSPORT_B_PORT"
  export QM_CLUSTER_LOCAL_ADDR="$TRANSPORT_B"
  exec "$QM_BIN" --data-dir "$DATA_B" start --foreground --host 127.0.0.1 --port "$PG_PORT_B" --admin-password certify-test
) &
PIDS+=($!)
wait_for_port "$TRANSPORT_B_PORT" "standby transport"

echo "=== Start primary (node A) transport :${TRANSPORT_A_PORT} ==="
(
  cluster_common_env
  export QM_CLUSTER_NODE_ID=1
  export QM_CLUSTER_TRANSPORT_PORT="$TRANSPORT_A_PORT"
  export QM_CLUSTER_LOCAL_ADDR="$TRANSPORT_A"
  exec "$QM_BIN" --data-dir "$DATA_A" start --foreground --host 127.0.0.1 --port "$PG_PORT_A" --admin-password certify-test
) &
PIDS+=($!)
wait_for_port "$TRANSPORT_A_PORT" "primary transport"

echo "=== qm cluster certify (primary env) ==="
(
  cluster_common_env
  export QM_CLUSTER_NODE_ID=1
  export QM_CLUSTER_TRANSPORT_PORT="$TRANSPORT_A_PORT"
  export QM_CLUSTER_LOCAL_ADDR="$TRANSPORT_A"
  "$QM_BIN" --data-dir "$DATA_A" cluster certify
)

echo "PASS: run_cluster_certify_live"
