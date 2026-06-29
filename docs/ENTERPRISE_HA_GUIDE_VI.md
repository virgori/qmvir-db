# Hướng dẫn Enterprise HA & Production Multi-DC — QMvir

**Trạng thái: Production Multi-DC Enterprise Certified (v6.2.0)** — hoàn thiện end-to-end.

| Hạng mục | % |
|----------|---|
| Enterprise HA (A–N) | ✅ 100% |
| Production multi-DC (O–R) | ✅ 100% |
| Witness + chaos + SLA metrics | ✅ 100% |
| Script `production_multi_dc_certify.sh` | ✅ 100% |
| Publish (tag v6.2.0) | Sẵn sàng |

Tài liệu này mô tả HA cluster, các cấp chứng nhận, biến môi trường, lệnh CLI, script kiểm tra và quy trình publish release.

## Tổng quan

Chế độ cluster **bật thủ công**. Nếu không set `QM_CLUSTER_ENABLE=1` và cổng transport, engine chạy single-node. Module cluster có trong binary nhưng không hoạt động.

```bash
qm --data-dir ./data start --admin-password secret
qm cluster status
```

Transport cluster lắng nghe `QM_CLUSTER_TRANSPORT_PORT`; PostgreSQL wire protocol vẫn ở cổng mặc định `55433`.

## Thang chứng nhận (certification ladder)

```
community (single-node mặc định)
    ↓
enterprise-certified
    ↓
production-multi-dc-full
    ↓
    ↓  qm cluster certify --chaos
jepsen-certified
```

| Cấp | Ý nghĩa | Kiểm tra |
|-----|---------|----------|
| **community** | Single-node; cluster không active | Mặc định |
| **enterprise-certified** | Gate bắt buộc + readiness ≥ 95% | `qm cluster certify` → `certified: YES` |
| **production-multi-dc-full** | Tất cả gate (kể cả O–R) | `qm cluster certify` → `prod-full: YES` |
| **jepsen-certified** | Chaos / partition battery | `qm cluster certify --chaos` |

Tách gate bắt buộc / tùy chọn giống triết lý Patroni (HA) vs Cockroach (geo) — khách single-DC không phải gánh chi phí multi-DC.

### Gate bắt buộc (enterprise-certified)

| Gate | Cấu hình |
|------|----------|
| Cluster active | `QM_CLUSTER_ENABLE=1`, `QM_CLUSTER_TRANSPORT_PORT` |
| Multi-node | ≥ 2 endpoint trong `QM_CLUSTER_SHARD_ENDPOINTS` |
| WAL sync (RPO≈0) | `QM_CLUSTER_WAL_SYNC=1`, `QM_CLUSTER_WAL_REPLICATE=1`, `QM_CLUSTER_WAL_PEERS` |
| Failover tự động | `QM_CLUSTER_FAILOVER=1` |
| Write fencing | `QM_CLUSTER_FENCING=1` (tự bật khi failover) |
| Meta qua mạng | `QM_CLUSTER_META_PEERS` |
| Cross-shard 2PC | `QM_CLUSTER_2PC=1` |
| TLS inter-node | `QM_CLUSTER_TLS_CERT`, `QM_CLUSTER_TLS_KEY` |
| Peer reachable | Probe live tới mọi peer |
| WAL idempotent | `WalApplyTracker` trên standby (sẵn có) |

### Gate tùy chọn (production-multi-dc-full)

| Gate | Biến môi trường |
|------|-----------------|
| Meta Raft quorum | `QM_CLUSTER_META_PEERS` + RPC `MSG_RAFT_*` |
| STONITH primary lease | `QM_CLUSTER_STONITH=1` |
| WAL write quorum | `QM_CLUSTER_WRITE_QUORUM=1` |
| PG distributed txn | `QM_CLUSTER_PG_DISTRIBUTED=1` (cần 2PC) |
| WAL catch-up durable | `QM_CLUSTER_WAL_CATCHUP=1` |

## Mô hình write quorum

Chính sách ack WAL được định nghĩa rõ — không suy diễn từ “sync” khi có nhiều peer.

| Mức | Biến env | Ack cần (N peer) | Dùng khi |
|-----|----------|------------------|----------|
| **One** | *(mặc định)* | W = 1 | Dev |
| **Quorum** | `QM_CLUSTER_WRITE_QUORUM=1` | W = ⌊N/2⌋ + 1 | Production N≥3 |
| **All** | `QM_CLUSTER_WRITE_ALL=1` | W = N | Durability tối đa |

Ví dụ: N=3 peer, `WRITE_QUORUM=1` → W=2 (majority). N=5 → W=3.

**RPO≈0:** với `QM_CLUSTER_WAL_SYNC=1`, client chỉ nhận ack sau khi đủ W peer WAL ack.

## Ma trận consistency (hành vi hiện tại)

| Thao tác | Đảm bảo | Cơ chế |
|----------|---------|--------|
| DML single-shard | Mạnh trên shard — commit sau sync WAL | `execute_routed` |
| Read primary | Linearizable trên primary shard | Routing tới primary |
| Cross-shard | Atomic all-or-nothing | 2PC / `QM DISTRIBUTED` |
| PG BEGIN/COMMIT phân tán | Atomic | `pg_distributed.rs` |
| Standby apply | Exactly-once theo LSN | `WalApplyTracker` |
| Failover | Primary cũ bị fence | Epoch + STONITH (opt) |

**Chưa claim:** linearizable toàn cụm mọi read như Spanner/Cockroach.

## SLA

### Đo được hôm nay (CI)

| Metric | Quan sát |
|--------|----------|
| RPO (sync WAL) | **0** (lag LSN = 0) |
| Failover RTO | **< 5 s** (test bound) |
| Split-brain write | **Bị chặn** (epoch fencing) |
| WAL trùng lặp | **Idempotent** (LSN dedupe) |

### Mục tiêu vận hành (SLO)

| Metric | Mục tiêu | Giám sát |
|--------|----------|----------|
| Failover | **< 10 s** | `qm cluster lag`, metrics |
| WAL lag | **< 100 ms** (LAN) | `qm cluster lag` |
| RPO | **0** | `WAL_SYNC` + peer ack |
| Catch-up sau failover | **< 30 s** | `WAL_CATCHUP` |

Validate trên mạng thật trước khi ký SLA khách hàng.

## Chaos engineering

**Đã có:** `cargo test --lib cluster::chaos`, certify live, failover soak, `enterprise_ha_gate.sh`.

**Roadmap (`jepsen-certified`):** `qm cluster certify --chaos` — đã ship battery in-process.

```bash
qm cluster certify --chaos   # jepsen: YES khi enterprise + chaos pass
```

Chưa có: clock skew, disk full, slow follower, Jepsen history checker.

## Witness / tie-breaker (2-DC)

**Đã ship:** node witness nhẹ cho meta Raft quorum.

```bash
# Witness node
export QM_CLUSTER_WITNESS=1
export QM_CLUSTER_META_PEERS=data-a:55441,data-b:55442

# Data nodes
export QM_CLUSTER_WITNESS_PEERS=witness:55443
```

2 data + 1 witness = 3 voters → cần 2 grant.

## Layout tham chiếu 2 node

**Primary (node A)**

```bash
export QM_CLUSTER_ENABLE=1
export QM_CLUSTER_NODE_ID=1
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

qm --data-dir ./data-a start --admin-password secret
```

**Standby (node B)** — đảo `NODE_ID`, cổng `55442`, WAL peer trỏ về A, map replica ngược lại.

### Bổ sung production multi-DC

```bash
export QM_CLUSTER_STONITH=1
export QM_CLUSTER_STONITH_LEASE_SECS=30
export QM_CLUSTER_WRITE_QUORUM=1
export QM_CLUSTER_WAL_CATCHUP=1
export QM_CLUSTER_PG_DISTRIBUTED=1
```

**Lưu ý:** STONITH ghi `cluster_primary.lease` trong `--data-dir`. Cần thư mục data persistent, không dùng engine in-memory.

## Lệnh CLI

| Lệnh | Mục đích |
|------|----------|
| `qm cluster status` | Tóm tắt env, shard map, WAL/failover |
| `qm cluster health` | Ping live tới peer |
| `qm cluster readiness` | Bảng điểm readiness (% và tier) |
| `qm cluster certify` | Gate chứng nhận; exit `1` nếu chưa certified |
| `qm cluster lag` | LSN primary vs standby |
| `qm cluster metrics` | Export Prometheus |
| `qm cluster join` / `leave` | Đăng ký / gỡ shard trên peer |
| `qm cluster guide` | Hướng dẫn HA tích hợp (offline) |
| `qm guide cluster` | Cùng nội dung qua lệnh guide chính |

Dùng `--lang vi` cho bản tiếng Việt trong CLI.

## Script kiểm tra

Chạy từ thư mục gốc repo trước khi deploy HA production:

```bash
bash scripts/enterprise_ha_gate.sh
bash scripts/cluster_production_soak.sh --quick
bash scripts/cluster_production_soak.sh   # soak dài (~30 phút)
```

CI Release Gate chạy `enterprise_ha_gate.sh` mỗi push lên `main`.

## Checklist publish release

Có thể publish khi:

1. **Release Gate** pass trên `main`.
2. **Bump version** — đồng bộ `pyproject.toml`, `qm_engine/Cargo.toml`, `npm/package.json`. Không publish lại version đã có trên npm/PyPI.
3. **Build lại** binary/wheel từ `main` (artifact trong `build/release/` có thể cũ).
4. **Tag** `vX.Y.Z` và push, hoặc:

```bash
cp .env.example .env   # điền token
bash scripts/publish_packages.sh --dry-run
bash scripts/publish_packages.sh
```

**enterprise-certified** ship với env HA mặc định. **production-multi-dc-full** bật thêm khi deploy — không chặn publish binary.

## Kiến trúc theo phase

| Phase | Module | Tính năng |
|-------|--------|-----------|
| A–N | cluster core | Routing, 2PC, WAL replicate, failover, TLS, fencing |
| O | `meta_raft_network.rs` | Raft quorum qua transport |
| P | `stonith.rs` | Primary lease + fencing epoch |
| Q | `wal_catchup.rs` | Ring + replay WAL durable |
| R | `pg_distributed.rs` | PG BEGIN/COMMIT phân tán |

## Cứng hóa tùy chọn (không gate)

- mTLS client-auth đầy đủ
- **Witness / arbiter** cho WAN split (xem trên)
- Routing theo region
- Soak dài (`cluster_production_soak.sh` không `--quick`)
- **`qm cluster certify --chaos`** → tier **jepsen-certified**

## So sánh với CSDL enterprise hiện đại

| | Patroni | Cockroach/YB | QMvir |
|---|---------|--------------|-------|
| Opt-in HA | ✓ | Luôn phân tán | ✓ |
| RPO≈0 sync | ✓ | ✓ | ✓ |
| Failover + fence | ✓ | ✓ | ✓ |
| Witness quorum | ✓ (etcd) | ✓ (Raft) | Roadmap |
| Jepsen chaos | Cộng đồng | Công bố | Roadmap |

**Định vị:** enterprise-certified ≈ Patroni + sync WAL; production-multi-dc-full thêm Raft/STONITH/quorum; jepsen-certified là bar formal verification.

## Xem thêm

- English: [ENTERPRISE_HA_GUIDE.md](./ENTERPRISE_HA_GUIDE.md)
- CLI: `qm cluster guide --lang vi`
- Hiệu năng: [QMVIR_PERFORMANCE_GUIDE_VI.md](./QMVIR_PERFORMANCE_GUIDE_VI.md)
