#!/usr/bin/env bash
# Enterprise HA release gate — run before marketing / production HA deploy.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT/qm_engine"

# Live certify / manual runs may leave cluster env in the shell; clear before tests.
while IFS='=' read -r k _; do
  if [[ "$k" == QM_CLUSTER_* ]]; then
    unset "$k" || true
  fi
done < <(env)

echo "=== [1/6] Single-node isolation (no HA regression) ==="
cargo test -q --no-default-features --test single_node_cluster_isolation -- --test-threads=1

echo "=== [2/6] Cluster HA lib tests (chaos, failover, WAL) ==="
cargo test -q --no-default-features --lib cluster:: -- --test-threads=1

echo "=== [3/6] Cluster integration tests ==="
cargo test -q --no-default-features --test cluster_ha_smoke --test cluster_gateway_forward -- --test-threads=1

echo "=== [4/6] Enterprise certify live (2-node in-process) ==="
cargo test -q --no-default-features --test cluster_certify_live -- --test-threads=1

echo "=== [5/6] Enterprise certify CLI (live 2-node processes) ==="
bash "$ROOT/scripts/run_cluster_certify_live.sh"

echo "=== [6/6] Failover soak drill (live cluster) ==="
bash "$ROOT/scripts/cluster_failover_soak.sh" --quick

echo ""
echo "=== Enterprise HA env checklist (all required for certify) ==="
cat <<'EOF'
export QM_CLUSTER_ENABLE=1
export QM_CLUSTER_TRANSPORT_PORT=55441
export QM_CLUSTER_LOCAL_ADDR=127.0.0.1:55441
export QM_CLUSTER_SHARD_ENDPOINTS=0=127.0.0.1:55441,1=127.0.0.1:55441,2=127.0.0.1:55442,3=127.0.0.1:55442
export QM_CLUSTER_SHARD_REPLICAS=0=127.0.0.1:55442,2=127.0.0.1:55442
export QM_CLUSTER_WAL_REPLICATE=1
export QM_CLUSTER_WAL_SYNC=1
export QM_CLUSTER_WAL_PEERS=127.0.0.1:55442
export QM_CLUSTER_FAILOVER=1
export QM_CLUSTER_FENCING=1
export QM_CLUSTER_2PC=1
export QM_CLUSTER_META_PEERS=127.0.0.1:55442
export QM_CLUSTER_TLS_CERT=/path/to/cert.pem
export QM_CLUSTER_TLS_KEY=/path/to/key.pem
EOF

echo ""
echo "PASS: enterprise_ha_gate"
