# QMvir 6.0 — Giáo trình Tận dụng Hiệu năng

> Hướng dẫn thực hành để đạt throughput/latency tốt nhất với Native SQL engine (Rust).
> Phiên bản: **6.0.0** · Cập nhật: 2026-06-24

---

## 1. Nguyên tắc chung

| Nguyên tắc | Lý do |
|------------|-------|
| **Tạo index trước khi query nặng** | OLAP, FTS, trigram, HNSW đều có fast path chỉ khi index đã build |
| **Batch INSERT thay vì autocommit từng row** | Giảm parse/WAL overhead; multivalue `VALUES (...),(...)` nhanh hơn 10–50× |
| **Chạy `CREATE INDEX` sau bulk load** | HNSW `batch_build` tối ưu cho graph ≤8K vectors/lần |
| **Dùng columnar-friendly SQL** | `GROUP BY` trên cột int nhỏ (`i % 100`), `COUNT(*)` filter trên int index |
| **So sánh benchmark công bằng** | Dùng cùng deployment (native hoặc Docker) — xem mục 7 |

---

## 2. OLAP (analytics in-memory)

### 2.1 Pattern nhanh nhất

```sql
-- Full scan COUNT — columnar, không sort
SELECT COUNT(*) FROM olap_bench;

-- Filter trên int column (grp = i % 100)
SELECT COUNT(*) FROM olap_bench WHERE grp = 42;

-- GROUP BY + SUM trên int key nhỏ
SELECT grp, SUM(val) AS s
FROM olap_bench
GROUP BY grp
ORDER BY grp;
```

Engine tự dùng **columnar cache** (`int_cols`, `float_cols`) và dense parallel accumulator khi group key ∈ [0, 4096).

### 2.2 Tránh

```sql
-- Chậm: ORDER BY trước GROUP BY (đã fix trong 6.0, nhưng vẫn tránh viết sai thứ tự)
SELECT grp, COUNT(*) FROM t ORDER BY grp GROUP BY grp;  -- không dùng

-- Chậm: GROUP BY trên TEXT khi có thể dùng INT
SELECT tags, COUNT(*) FROM t GROUP BY tags;  -- OK nhỏ; lớn thì cân nhắc int bucket
```

### 2.3 Benchmark vs DuckDB

```bash
python scripts/compare_duckdb_olap_bench.py --rows 1000000 --iterations 30
```

Kết quả mục tiêu @1M rows: **5/5 workloads thắng DuckDB** (group_by_sum ~4ms vs ~5ms trong Docker).

---

## 3. Vector search (HNSW)

### 3.1 Quy trình khuyến nghị

```python
import qm_engine

DIM = 32

def vec_literal(i: int, dim: int = DIM) -> str:
    return "[" + ",".join(f"{((i + j) % 17) / 17.0:.6f}" for j in range(dim)) + "]"

qm = qm_engine.NativeSqlEngine()
qm.execute(f"CREATE TABLE docs (id INTEGER PRIMARY KEY, embedding VECTOR({DIM}))")

# 1) Bulk insert (multivalue)
values = ",".join(f"({i}, '{vec_literal(i)}')" for i in range(2000))
qm.execute(f"INSERT INTO docs (id, embedding) VALUES {values}")

# 2) Build index một lần
qm.execute("CREATE INDEX idx_docs_hnsw ON docs (embedding) USING hnsw")

# 3) Query KNN
qm.execute("SELECT id FROM docs ORDER BY embedding <-> '[0.1,0.2,...]' LIMIT 10")
```

### 3.2 Tuning recall / latency

| Tham số | Mặc định | Ghi chú |
|---------|----------|---------|
| `m` | 16 | Khớp pgvector/Qdrant bench |
| `ef_construction` | 200 (query), **64 (bulk build ≤8K)** | Bulk build tự giảm để tăng throughput |
| `ef_search` | `max(40, LIMIT k)` | Tăng nếu cần recall cao hơn |

Sweep ef_search trong Python:

```python
ids = qm.bench_hnsw_knn_l2("docs", "embedding", query_vec, top_k=10, ef_search=100)
```

### 3.3 Batch insert throughput

- Dùng **một câu INSERT** với nhiều `VALUES` (≤2000 rows/lần trong bench)
- Vector literal được parse song song (≥128 rows)
- **Lazy vector text**: không format `[...]` string khi insert — chỉ khi cần hiển thị

Benchmark vs Qdrant (fair Docker):

```bash
bash scripts/run_quizzman_docker_fair_bench.sh
```

---

## 4. Full-text & trigram search

### 4.1 Setup index

```sql
CREATE TABLE articles (id INTEGER PRIMARY KEY, body TEXT);
CREATE INDEX idx_body_fts ON articles (body) USING gin;
CREATE INDEX idx_body_trgm ON articles (body) USING gin_trgm;
```

### 4.2 Query patterns

```sql
-- FTS (inverted index)
SELECT id FROM articles WHERE body @@ 'machine learning';

-- LIKE contains — dùng columnar scan + trigram khi selective
SELECT id FROM articles WHERE body LIKE '%needle%';

-- Equality trên indexed column
SELECT id FROM articles WHERE tags = 'tag_42';
```

`LIKE '%literal%'` tự chọn:
- **Trigram intersection** khi selective (<20% rows)
- **Columnar parallel scan** khi full scan (≥256 rows)

---

## 5. JSON path filter

```sql
CREATE INDEX idx_data_name ON json_bench (data) USING json_path('name');
SELECT id FROM json_bench WHERE data->>'name' = 'user42';
```

---

## 6. Backup, dump & restore

### 6.1 Python API

```python
import qm_engine

engine = qm_engine.NativeSqlEngine()
# ... load data ...

# Backup (.qmvb)
qm_engine.backup(engine, "/data/backup.qmvb", compression="lz4")

# Verify
assert qm_engine.backup_verify("/data/backup.qmvb")["ok"]

# Restore vào engine mới
fresh = qm_engine.NativeSqlEngine()
qm_engine.backup_restore(fresh, "/data/backup.qmvb", drop_existing=True)
```

### 6.2 CLI (`qm` / `qmvir`)

```bash
qm backup -o /data/backup.qmvb --data-dir ./data
qm verify /data/backup.qmvb
qm restore -i /data/backup.qmvb --data-dir ./data_restored

# Dump (SQL / CSV / JSONL)
qm dump -t my_table -f sql --data-dir ./data
qm dump -t my_table -f csv -o /tmp/out.csv --data-dir ./data
```

### 6.3 Vector columns

6.0 hỗ trợ `VECTOR` trong pg_dump import và schema migration — backup/restore giữ nguyên vector payload.

### 6.4 Kiểm tra nhanh

```bash
pytest tests/test_backup_suite.py -q
```

---

## 7. Benchmark công bằng (Docker)

Để publish kết quả so sánh với Qdrant:

```bash
# Trên server bench (ví dụ quizzman)
export QM_DEPLOYMENT=docker_bridge
export QDRANT_DEPLOYMENT=docker_bridge
bash scripts/run_quizzman_docker_fair_bench.sh
```

`fairness_mode=all_container` → `publish_ready: true` trong JSON output.

Artifacts: `/tmp/qm_qdrant_docker_*.json`, `/tmp/qm_duckdb_docker_*.json`

---

## 8. WAL & durable writes (OLTP)

Chính sách sync (gọi qua Python API sau khi mở engine persistent):

| Policy | Ý nghĩa |
|--------|---------|
| `per_commit_sync` / `per_commit` | fsync mỗi COMMIT — an toàn nhất, chậm nhất |
| `group_commit_sync` / `group_commit` | group commit engine-native — cân bằng throughput/durability |
| `per_commit_sync_data` | sync data, không sync metadata dir |
| `relaxed_os_buffered` | OS buffer — bench/calibration only |
| `append_only_profile` | profile mode, không đảm bảo durability |

```python
qm = qm_engine.NativeSqlEngine()
qm.set_wal_sync_policy("group_commit_sync")  # alias: "group_commit"
```

> Chưa có biến môi trường `QM_WAL_SYNC_POLICY` — phải gọi `set_wal_sync_policy()` trong code.

Benchmark durable writes:

```bash
# QM-only profiler (per-commit / relaxed)
python scripts/native_sql_write_profile.py --output /tmp/qm_write_profile.json

# So sánh group-commit với PostgreSQL (concurrent writers)
python scripts/compare_postgres_group_commit.py --output /tmp/qm_pg_group_commit.json

# So sánh tổng hợp với PostgreSQL (persistent WAL)
python scripts/compare_postgres_native_sql.py \
  --qm-mode persistent-wal \
  --qm-sync-policy group-commit \
  --sync-every-n 64
```

---

## 9. Checklist trước production

- [ ] Index đã build cho mọi query pattern nóng
- [ ] Bulk load → `CREATE INDEX` (không insert từng row có index)
- [ ] Backup `.qmvb` định kỳ + `backup verify`
- [ ] Benchmark segment chạy với deployment metadata đúng
- [ ] Recall@k đo với corpus unique (không mod-17) cho vector

---

## 10. Tham chiếu script

| Script | Mục đích |
|--------|----------|
| `scripts/compare_duckdb_olap_bench.py` | OLAP vs DuckDB |
| `scripts/compare_qdrant_vector_bench.py` | Vector vs Qdrant |
| `scripts/compare_postgres_search_bench.py` | Search vs PostgreSQL |
| `scripts/run_quizzman_docker_fair_bench.sh` | Fair all-container suite |
| `scripts/vector_recall_bench.py` | Recall@10/50/100 |
| `scripts/run_segment_benchmark_suite.py` | Tổng hợp segment |
| `scripts/native_sql_write_profile.py` | Profile ghi durable (QM-only) |
| `scripts/compare_postgres_group_commit.py` | Group-commit vs PostgreSQL |

---

*Tài liệu liên quan: [USAGE_GUIDE_VI.md](../USAGE_GUIDE_VI.md) (hướng dẫn tổng quát), [USAGE_GUIDE_EN.md](../USAGE_GUIDE_EN.md).*
