# Shared helpers for live 2-node HA scripts (certify, soak).
# shellcheck shell=bash
[[ -n "${CLUSTER_HA_LIB_LOADED:-}" ]] && return 0
CLUSTER_HA_LIB_LOADED=1

HA_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
HA_QM_ENGINE="$HA_ROOT/qm_engine"
HA_QM_BIN="${QM_BIN:-}"
HA_TRANSPORT_A_PORT="${TRANSPORT_A_PORT:-55441}"
HA_TRANSPORT_B_PORT="${TRANSPORT_B_PORT:-55442}"
HA_PG_PORT_A="${PG_PORT_A:-55433}"
HA_PG_PORT_B="${PG_PORT_B:-55434}"
HA_TLS_DIR="${TLS_DIR:-$HA_ROOT/.cluster-tls-dev}"
HA_WORKDIR="${WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/qm-cluster-ha.XXXXXX")}"
HA_TLS_CERT=""
HA_TLS_KEY=""
HA_TRANSPORT_A=""
HA_TRANSPORT_B=""
HA_DATA_A=""
HA_DATA_B=""
HA_PID_A=""
HA_PID_B=""
HA_PIDS=()

ha_cleanup() {
  for pid in "${HA_PIDS[@]}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
  if [[ "${KEEP_WORKDIR:-0}" != "1" ]]; then
    rm -rf "$HA_WORKDIR"
  fi
}

ha_port_open() {
  if command -v nc >/dev/null 2>&1; then
    nc -z 127.0.0.1 "$1" 2>/dev/null
    return $?
  fi
  (echo >/dev/tcp/127.0.0.1/"$1") >/dev/null 2>&1
}

ha_wait_for_port() {
  local port=$1
  local label=$2
  for _ in $(seq 1 60); do
    if ha_port_open "$port"; then
      return 0
    fi
    sleep 0.25
  done
  echo "ERROR: $label port $port did not open" >&2
  return 1
}

ha_resolve_qm_bin() {
  local target_dir
  if command -v python3 >/dev/null 2>&1; then
    target_dir="$(
      cargo metadata --manifest-path "$HA_QM_ENGINE/Cargo.toml" --format-version 1 \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['target_directory'])"
    )"
  else
    target_dir="$(
      cargo metadata --manifest-path "$HA_QM_ENGINE/Cargo.toml" --format-version 1 \
        | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' | head -1
    )"
  fi
  if [[ -x "${HA_QM_BIN:-}" ]]; then
    return 0
  fi
  if [[ -x "$target_dir/debug/qm" ]]; then
    HA_QM_BIN="$target_dir/debug/qm"
  elif [[ -x "$target_dir/release/qm" ]]; then
    HA_QM_BIN="$target_dir/release/qm"
  elif [[ -x "$HA_QM_ENGINE/target/debug/qm" ]]; then
    HA_QM_BIN="$HA_QM_ENGINE/target/debug/qm"
  elif [[ -x "$HA_QM_ENGINE/target/release/qm" ]]; then
    HA_QM_BIN="$HA_QM_ENGINE/target/release/qm"
  else
    echo "ERROR: qm binary not found" >&2
    return 1
  fi
}

ha_build_qm() {
  cargo build --manifest-path "$HA_QM_ENGINE/Cargo.toml" --no-default-features --bin qm -q
  ha_resolve_qm_bin
}

ha_generate_tls() {
  bash "$HA_ROOT/scripts/generate_cluster_tls_dev.sh" "$HA_TLS_DIR" >/dev/null
  HA_TLS_CERT="$HA_TLS_DIR/cluster-dev.crt"
  HA_TLS_KEY="$HA_TLS_DIR/cluster-dev.key"
}

ha_init_paths() {
  HA_TRANSPORT_A="127.0.0.1:${HA_TRANSPORT_A_PORT}"
  HA_TRANSPORT_B="127.0.0.1:${HA_TRANSPORT_B_PORT}"
  HA_DATA_A="$HA_WORKDIR/node-a"
  HA_DATA_B="$HA_WORKDIR/node-b"
  mkdir -p "$HA_DATA_A" "$HA_DATA_B"
}

ha_export_common_env() {
  export QM_CLUSTER_ENABLE=1
  export QM_CLUSTER_SHARD_ENDPOINTS="0=${HA_TRANSPORT_A},1=${HA_TRANSPORT_A},2=${HA_TRANSPORT_B},3=${HA_TRANSPORT_B}"
  export QM_CLUSTER_SHARD_REPLICAS="0=${HA_TRANSPORT_B},2=${HA_TRANSPORT_B}"
  export QM_CLUSTER_WAL_REPLICATE=1
  export QM_CLUSTER_WAL_SYNC=1
  export QM_CLUSTER_WAL_PEERS="${HA_TRANSPORT_B}"
  export QM_CLUSTER_FAILOVER=1
  export QM_CLUSTER_FENCING=1
  export QM_CLUSTER_2PC=1
  export QM_CLUSTER_META_PEERS="${HA_TRANSPORT_B}"
  export QM_CLUSTER_TLS_CERT="$HA_TLS_CERT"
  export QM_CLUSTER_TLS_KEY="$HA_TLS_KEY"
  export QM_CLUSTER_TLS_CA="$HA_TLS_CERT"
  export QM_ADMIN_PASSWORD="${QM_ADMIN_PASSWORD:-certify-test}"
}

ha_start_standby() {
  (
    ha_export_common_env
    export QM_CLUSTER_NODE_ID=2
    export QM_CLUSTER_TRANSPORT_PORT="$HA_TRANSPORT_B_PORT"
    export QM_CLUSTER_LOCAL_ADDR="$HA_TRANSPORT_B"
    exec "$HA_QM_BIN" --data-dir "$HA_DATA_B" start --foreground --host 127.0.0.1 \
      --port "$HA_PG_PORT_B" --admin-password "${QM_ADMIN_PASSWORD:-certify-test}"
  ) &
  HA_PID_B=$!
  HA_PIDS+=("$HA_PID_B")
  ha_wait_for_port "$HA_TRANSPORT_B_PORT" "standby transport"
}

ha_start_primary() {
  (
    ha_export_common_env
    export QM_CLUSTER_NODE_ID=1
    export QM_CLUSTER_TRANSPORT_PORT="$HA_TRANSPORT_A_PORT"
    export QM_CLUSTER_LOCAL_ADDR="$HA_TRANSPORT_A"
    exec "$HA_QM_BIN" --data-dir "$HA_DATA_A" start --foreground --host 127.0.0.1 \
      --port "$HA_PG_PORT_A" --admin-password "${QM_ADMIN_PASSWORD:-certify-test}"
  ) &
  HA_PID_A=$!
  HA_PIDS+=("$HA_PID_A")
  ha_wait_for_port "$HA_TRANSPORT_A_PORT" "primary transport"
}

ha_start_nodes() {
  ha_init_paths
  ha_start_standby
  ha_start_primary
}

ha_stop_primary() {
  if [[ -n "$HA_PID_A" ]]; then
    kill "$HA_PID_A" 2>/dev/null || true
    wait "$HA_PID_A" 2>/dev/null || true
    HA_PID_A=""
  fi
}

ha_with_primary_env() {
  (
    ha_export_common_env
    export QM_CLUSTER_NODE_ID=1
    export QM_CLUSTER_TRANSPORT_PORT="$HA_TRANSPORT_A_PORT"
    export QM_CLUSTER_LOCAL_ADDR="$HA_TRANSPORT_A"
    "$@"
  )
}
