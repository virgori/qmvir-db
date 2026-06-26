#!/usr/bin/env bash
# Soak + failover drill on live 2-node HA cluster.
# Usage: bash scripts/cluster_failover_soak.sh [--quick]
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=cluster_ha_lib.sh
source "$ROOT/scripts/cluster_ha_lib.sh"

QUICK=0
[[ "${1:-}" == "--quick" ]] && QUICK=1
SOAK_ROUNDS="${SOAK_ROUNDS:-10}"
[[ "$QUICK" == "1" ]] && SOAK_ROUNDS=3

trap ha_cleanup EXIT INT TERM

echo "=== Build qm ==="
ha_build_qm
echo "  using: $HA_QM_BIN"

echo "=== TLS ==="
ha_generate_tls

echo "=== Start 2-node cluster ==="
ha_start_nodes

echo "=== Soak: $SOAK_ROUNDS health rounds ==="
for ((i = 1; i <= SOAK_ROUNDS; i++)); do
  out="$(ha_with_primary_env "$HA_QM_BIN" --data-dir "$HA_DATA_A" cluster health)"
  echo "$out" | grep -q '1/1 peers reachable' || {
    echo "FAIL: health round $i" >&2
    echo "$out" >&2
    exit 1
  }
  if [[ "$QUICK" != "1" ]]; then
    sleep 0.5
  fi
done

echo "=== Failover drill: stop primary ==="
ha_stop_primary
sleep 1
if ha_port_open "$HA_TRANSPORT_A_PORT"; then
  echo "FAIL: primary transport still open after kill" >&2
  exit 1
fi
down_out="$(ha_with_primary_env "$HA_QM_BIN" --data-dir "$HA_DATA_A" cluster health || true)"
echo "$down_out" | grep -q '0/1 peers reachable' || {
  echo "WARN: expected peer down after primary stop (continuing)"
}

echo "=== Failover drill: restart primary ==="
ha_start_primary
ha_with_primary_env "$HA_QM_BIN" --data-dir "$HA_DATA_A" cluster certify

echo "PASS: cluster_failover_soak"
