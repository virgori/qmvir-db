# Hướng dẫn Enterprise HA & Production Multi-DC — QMvir

Tài liệu này mô tả HA cluster, hai cấp chứng nhận, biến môi trường, lệnh CLI, script kiểm tra và quy trình publish release.

## Tổng quan

Chế độ cluster **bật thủ công**. Nếu không set `QM_CLUSTER_ENABLE=1` và cổng transport, engine chạy single-node. Module cluster có trong binary nhưng không hoạt động.

```bash
qm --data-dir ./data start --admin-password secret
qm cluster status
```

Transport cluster lắng nghe `QM_CLUSTER_TRANSPORT_PORT`; PostgreSQL wire protocol vẫn ở cổng mặc định `55433`.

## Hai cấp chứng nhận

| Cấp | Ý nghĩa | Kiểm tra |
|-----|---------|----------|
| **enterprise-certified** | Gate bắt buộc pass + điểm readiness ≥ 95% | `qm cluster certify` → `certified: YES` |
| **production-multi-dc-full** | Tất cả gate (kể cả O–R tùy chọn) | `qm cluster certify` → `prod-full: YES` |

Trong output `qm cluster certify`: `(req)` = bắt buộc cho enterprise; `(opt)` = cần pass hết để đạt production-multi-dc-full.

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

- mTLS client-auth đầy đủ giữa các node
- Witness cross-DC / routing theo region
- Soak production dài (không `--quick`)

## Xem thêm

- English: [ENTERPRISE_HA_GUIDE.md](./ENTERPRISE_HA_GUIDE.md)
- CLI: `qm cluster guide --lang vi`
- Hiệu năng: [QMVIR_PERFORMANCE_GUIDE_VI.md](./QMVIR_PERFORMANCE_GUIDE_VI.md)
