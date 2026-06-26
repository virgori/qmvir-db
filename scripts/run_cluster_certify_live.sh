#!/usr/bin/env bash
# Start two qm processes (HA transport + TLS), run `qm cluster certify`, tear down.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# shellcheck source=cluster_ha_lib.sh
source "$ROOT/scripts/cluster_ha_lib.sh"

trap ha_cleanup EXIT INT TERM

echo "=== Build qm (no python) ==="
ha_build_qm
echo "  using: $HA_QM_BIN"

echo "=== Dev TLS certs ==="
ha_generate_tls

echo "=== Start standby (node B) transport :${HA_TRANSPORT_B_PORT} ==="
ha_init_paths
ha_start_standby

echo "=== Start primary (node A) transport :${HA_TRANSPORT_A_PORT} ==="
ha_start_primary

echo "=== qm cluster certify (primary env) ==="
ha_with_primary_env "$HA_QM_BIN" --data-dir "$HA_DATA_A" cluster certify

echo "PASS: run_cluster_certify_live"
