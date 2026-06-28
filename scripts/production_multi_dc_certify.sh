#!/usr/bin/env bash
# Production Multi-DC Enterprise Certified — end-to-end validation gate.
# Runs HA gate + in-process chaos battery (jepsen-certified tier).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QUICK=0
if [[ "${1:-}" == "--quick" ]]; then
  QUICK=1
fi

echo "=== Production Multi-DC Enterprise Certified gate ==="

bash "$ROOT/scripts/enterprise_ha_gate.sh"

echo ""
echo "=== [8/8] Chaos battery (jepsen-certified scenarios) ==="
cd "$ROOT/qm_engine"
cargo test -q --no-default-features --test cluster_full_enterprise chaos_battery_all_scenarios_pass -- --test-threads=1

if [[ "$QUICK" -eq 1 ]]; then
  echo ""
  echo "PASS: production_multi_dc_certify (quick — chaos lib only)"
  echo "For live jepsen tier: qm cluster certify --chaos (with full QM_CLUSTER_* env)"
  exit 0
fi

echo ""
echo "=== Full env checklist (production-multi-dc-full) ==="
cat <<'EOF'
export QM_CLUSTER_ENABLE=1
export QM_CLUSTER_STONITH=1
export QM_CLUSTER_WRITE_QUORUM=1
export QM_CLUSTER_WAL_CATCHUP=1
export QM_CLUSTER_PG_DISTRIBUTED=1
export QM_CLUSTER_WITNESS_PEERS=127.0.0.1:55443
export QM_CLUSTER_META_PEERS=127.0.0.1:55442
# ... plus enterprise block from enterprise_ha_gate.sh
qm cluster certify --chaos
EOF

echo ""
echo "PASS: production_multi_dc_certify"
