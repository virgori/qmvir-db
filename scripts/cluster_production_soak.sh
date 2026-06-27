#!/usr/bin/env bash
# Production multi-DC soak — extended HA validation (optional, --quick for CI).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
QUICK=0
if [[ "${1:-}" == "--quick" ]]; then
  QUICK=1
fi

echo "=== Production multi-DC soak (quick=$QUICK) ==="

cd "$ROOT/qm_engine"

echo "[1/4] Full enterprise lib tests"
cargo test -q --no-default-features --lib 'cluster::meta_raft_network|cluster::stonith|cluster::wal_catchup|cluster::pg_distributed' -- --test-threads=1

echo "[2/4] Full enterprise integration tests"
cargo test -q --no-default-features --test cluster_full_enterprise -- --test-threads=1

if [[ "$QUICK" -eq 1 ]]; then
  echo "[3/4] Skipping long soak (--quick)"
  echo "[4/4] Skipping long soak (--quick)"
  echo "PASS: cluster_production_soak (quick)"
  exit 0
fi

echo "[3/4] Extended failover soak (30 min)"
bash "$ROOT/scripts/cluster_failover_soak.sh"

echo "[4/4] Re-run enterprise HA gate"
bash "$ROOT/scripts/enterprise_ha_gate.sh"

echo "PASS: cluster_production_soak (full)"
