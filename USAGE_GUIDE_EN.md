# QMvir — Usage Guide

**Version:** v6.0.0  
**Language:** Rust — standalone binary, no Python/Java required  
**Protocol:** PostgreSQL wire protocol — compatible with `psql`, JDBC, any PG client  
**Default CLI language:** English (`--lang en`). Also supports: `vi`, `zht`, `zh`  
**Performance tuning:** see [QMVIR_PERFORMANCE_GUIDE_VI.md](docs/QMVIR_PERFORMANCE_GUIDE_VI.md) (Vietnamese; OLAP/vector/search/WAL benchmarks)

---

## Table of Contents

1. [Installation](#1-installation)
2. [Quick Start](#2-quick-start)
3. [Basic SQL](#3-basic-sql)
4. [Data Types](#4-data-types)
5. [Index](#5-index)
6. [Full-Text Search](#6-full-text-search)
7. [Vector Search](#7-vector-search)
8. [Backup & Restore](#8-backup--restore)
9. [Web Dashboard & REST API](#9-web-dashboard--rest-api)
10. [JavaScript / TypeScript SDK](#10-javascript--typescript-sdk)
11. [Advanced Configuration](#11-advanced-configuration)
12. [Statistics & Monitoring](#12-statistics--monitoring)
13. [Cluster & Sharding](#13-cluster--sharding)
14. [Security](#14-security)
15. [Benchmark](#15-benchmark)
16. [CLI Reference](#16-cli-reference)

---

## 1. Installation

### npm (recommended — auto-downloads binary)

```bash
npm install -g qmvir
```

Postinstall automatically downloads the correct binary for your platform (~7–8 MB). Then use `qm` or `qmvir` command.

### Direct binary download

```bash
# Linux x86-64
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.0.0/qm-linux-x86_64
chmod +x qm-linux-x86_64
sudo mv qm-linux-x86_64 /usr/local/bin/qm

# Linux ARM64
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.0.0/qm-linux-aarch64
chmod +x qm-linux-aarch64
sudo mv qm-linux-aarch64 /usr/local/bin/qm

# macOS Apple Silicon
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.0.0/qm-macos-arm64
chmod +x qm-macos-arm64
sudo mv qm-macos-arm64 /usr/local/bin/qm

# Windows x86-64
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.0.0/qm-windows-x86_64.exe

# Windows ARM64
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.0.0/qm-windows-aarch64.exe
```

### Build from source

```bash
# Requires: Rust 1.75+
cd QM/qm_engine
cargo build --release --no-default-features --bin qm
./target/release/qm --help
```

### Verify

```bash
qm version
# QMvir v6.0.0
# Engine: qm_engine (Rust)
# Build: release
```

---

## 2. Quick Start

### Step 1: Start the server

```bash
qm --data-dir ./mydb start --admin-password mypassword
```

The server starts as a **daemon** (background) by default, listening on PostgreSQL wire protocol at port 55433.

To run in the foreground instead:
```bash
qm --data-dir ./mydb start --admin-password mypassword --foreground
```

### Step 2: Connect via psql

```bash
psql -h 127.0.0.1 -p 55433 -U admin -d qm
# Enter password: mypassword
```

### Step 3: Create a table and insert data

```sql
CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    email TEXT,
    balance DOUBLE PRECISION DEFAULT 0.0,
    active BOOLEAN DEFAULT true,
    created_at TIMESTAMP DEFAULT NOW()
);

INSERT INTO users (id, name, email, balance)
VALUES (1, 'Alice', 'alice@example.com', 150.50);

INSERT INTO users (id, name, email, balance)
VALUES (2, 'Bob', 'bob@example.com', 320.00);

SELECT * FROM users;
```

### Step 4: Execute SQL directly from CLI (no server needed)

```bash
qm --data-dir ./mydb sql "SELECT * FROM users WHERE balance > 100"
```

### Step 5: Stop the server

```bash
qm stop
```

### Check server status

```bash
qm status
```

### Multi-language CLI

The CLI defaults to English. To change:
```bash
# Vietnamese (ASCII-safe, no diacritics)
qm --lang vi help

# Traditional Chinese
qm --lang zht help

# Simplified Chinese
qm --lang zh help

# Or set via environment variable
export QM_LANG=vi
qm help
```

---

## 3. Basic SQL

### 3.1 DDL — Create / alter / drop tables

```sql
-- Create table
CREATE TABLE products (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    price DOUBLE PRECISION,
    category TEXT,
    tags TEXT,
    created_at TIMESTAMP DEFAULT NOW()
);

-- Drop table
DROP TABLE products;

-- Delete all data (keep schema)
TRUNCATE products;
```

### 3.2 INSERT

```sql
-- Single row
INSERT INTO products (id, name, price, category)
VALUES (1, 'Laptop', 999.99, 'electronics');

-- Multiple rows
INSERT INTO products (id, name, price, category) VALUES
    (2, 'Phone', 699.00, 'electronics'),
    (3, 'Chair', 149.50, 'furniture'),
    (4, 'Desk', 299.00, 'furniture');
```

### 3.3 SELECT

```sql
-- Basic
SELECT * FROM products;
SELECT name, price FROM products WHERE category = 'electronics';

-- Sort
SELECT * FROM products ORDER BY price DESC;

-- Pagination
SELECT * FROM products ORDER BY id LIMIT 10 OFFSET 20;

-- Aggregate
SELECT category, COUNT(*) AS total, AVG(price) AS avg_price
FROM products
GROUP BY category
HAVING COUNT(*) > 1;
```

### 3.4 UPDATE

```sql
UPDATE products SET price = 899.00 WHERE id = 1;
UPDATE products SET active = false WHERE created_at < '2025-01-01';
```

### 3.5 DELETE

```sql
DELETE FROM products WHERE id = 4;
DELETE FROM products WHERE category = 'furniture' AND price < 200;
```

### 3.6 Transaction

```sql
BEGIN;
UPDATE accounts SET balance = balance - 100 WHERE id = 1;
UPDATE accounts SET balance = balance + 100 WHERE id = 2;
COMMIT;

-- Or rollback
BEGIN;
DELETE FROM products WHERE id = 1;
ROLLBACK;  -- Nothing deleted
```

### 3.7 EXPLAIN

```sql
EXPLAIN SELECT * FROM products WHERE category = 'electronics';
-- Shows query plan: sequential scan, index scan, etc.
```

---

## 4. Data Types

| Type | Description | Example |
|------|-------------|---------|
| `INTEGER` | 64-bit integer | `42`, `-100` |
| `DOUBLE PRECISION` | 64-bit float | `3.14`, `1e10` |
| `TEXT` | UTF-8 string | `'hello world'` |
| `BOOLEAN` | True/False | `true`, `false` |
| `TIMESTAMP` | Time (ms UTC) | `'2026-04-13T10:30:00'`, `NOW()` |
| `DATE` | Date (no time) | `'2026-04-13'` |
| `JSON` | Valid JSON data | `'{"key": "value"}'` |
| `BYTEA` | Binary data | `'\x48656c6c6f'` |
| `UUID` | 128-bit unique ID | `'550e8400-e29b-41d4-a716-446655440000'` |
| `NUMERIC` | Arbitrary precision number | `123456789.123456789` |
| `INTERVAL` | Time duration | `'1 hour'`, `'2 days'` |
| `ARRAY` | Array of values | `'{1, 2, 3}'` |
| `VECTOR(n)` | n-dimensional float32 vector | `[0.1, 0.2, 0.3, ...]` |

### Example table with multiple types

```sql
CREATE TABLE events (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    payload JSON,
    score NUMERIC,
    event_date DATE,
    duration INTERVAL,
    tags ARRAY,
    embedding VECTOR(384),
    active BOOLEAN DEFAULT true,
    created_at TIMESTAMP DEFAULT NOW()
);
```

---

## 5. Index

### 5.1 B+Tree (default)

Use for: exact search, range query, ORDER BY.

```sql
-- Create index
CREATE INDEX idx_email ON users (email);

-- Multi-column index
CREATE INDEX idx_cat_price ON products (category, price);

-- Drop
DROP INDEX idx_email;
```

Automatically created for `PRIMARY KEY`.

### 5.2 Inverted Index (full-text search)

```sql
CREATE INDEX idx_fulltext ON articles (title, body);
```

See [section 6](#6-full-text-search) for details.

### 5.3 HNSW (vector search)

```sql
CREATE INDEX idx_vec ON documents (embedding);
-- Or with options:
CREATE INDEX idx_vec ON documents (embedding)
    WITH (metric=cosine, m=16, ef_construction=200);
```

See [section 7](#7-vector-search) for details.

### 5.4 View indexes

```sql
SELECT * FROM information_schema.indexes;
```

---

## 6. Full-Text Search

QMvir uses the **BM25** algorithm with 3 scoring strategies:

| Strategy | When to use | Performance |
|----------|-------------|-------------|
| **DAAT** | Small datasets (< 100K docs) | Baseline |
| **WAND** | Medium datasets | ~2× faster than DAAT |
| **BMW** | Large datasets (> 1M docs) | ~5× faster than DAAT |

### 6.1 Create inverted index

```sql
CREATE TABLE articles (
    id INTEGER PRIMARY KEY,
    title TEXT,
    body TEXT,
    author TEXT
);

-- Index for full-text search
CREATE INDEX idx_search ON articles (title, body);
```

### 6.2 Search

```sql
-- Basic search (BM25 scoring automatic)
SELECT * FROM articles WHERE title MATCH 'database systems' TOP 10;

-- Search across multiple columns
SELECT * FROM articles WHERE body MATCH 'distributed' ORDER BY BM25_SCORE() DESC LIMIT 20;

-- Specify strategy
SELECT * FROM articles SEARCH 'machine learning' STRATEGY BMW TOP 100;
```

### 6.3 BM25 Parameters

Defaults: `k1 = 1.2`, `b = 0.75` — optimal for most cases.

- `k1` — Adjusts term frequency saturation. Higher → favor repeated terms more.
- `b` — Adjusts document length normalization. `1.0` = penalize long documents, `0.0` = ignore length.

### 6.4 Benchmark

| Scale | DAAT | BMW | Speedup |
|-------|------|-----|---------|
| 10K docs | 0.99 ms | 0.97 ms | ~1× |
| 1M docs | 148.4 ms | **66.8 ms** | **2.2×** |

Compared to PostgreSQL 17 (GIN + ts_rank): **153×** faster on 10K docs.

---

## 7. Vector Search

QMvir uses **HNSW** (Hierarchical Navigable Small World) with SIMD acceleration.

### 7.1 Create table + vector index

```sql
CREATE TABLE documents (
    id INTEGER PRIMARY KEY,
    title TEXT,
    embedding VECTOR(384)
);

-- Create HNSW index
CREATE INDEX idx_vec ON documents (embedding)
    WITH (metric=cosine, m=16, ef_construction=200);
```

### 7.2 Insert vector

```sql
INSERT INTO documents (id, title, embedding)
VALUES (1, 'Introduction to AI', [0.12, -0.34, 0.56, ...]);
```

Vector must have the exact number of dimensions (here: 384).

### 7.3 Vector search (k-NN)

```sql
-- Short syntax
LIKEV VEC [0.12, -0.34, 0.56, ...] IN documents TOP 10;

-- Full syntax
SEARCH VECTOR [0.12, -0.34, ...] IN documents TOP 10 METRIC cosine;

-- Get distance
SELECT DIST, id, title FROM documents
    WHERE embedding LIKEV [0.12, -0.34, ...] TOP 5;
```

### 7.4 Distance metric

| Metric | Formula | Use for |
|--------|---------|---------|
| `cosine` | 1 − (a·b)/(‖a‖‖b‖) | NLP embedding (default) |
| `l2` | √Σ(aᵢ − bᵢ)² | Images, coordinates |
| `ip` | −a·b | Recommendation |

### 7.5 HNSW Parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `m` | 16 | Max neighbors per node. Higher → better recall, more RAM |
| `ef_construction` | 200 | Build effort. Higher → better quality, slower build |
| `ef_search` | 100 | Search effort. Higher → better recall, higher latency |

### 7.6 Product Quantization (vector compression)

For large vector sets, use PQ for ~32× compression:

```sql
CREATE INDEX idx_pq ON vectors (embedding)
    WITH (type=hnsw_pq, metric=l2, pq_subspaces=8, pq_clusters=256);
```

Two-phase search: coarse search on PQ → re-rank with exact distance.

### 7.7 Hybrid Search (lexical + vector combined)

```sql
SEARCH HYBRID ON articles
    LEXICAL 'deep learning'
    VECTOR [0.12, -0.34, ...]
    TOP 10
    WEIGHTS LEXICAL 0.4 VECTOR 0.6;
```

### 7.8 Benchmark

| Metric | Value |
|--------|---------|
| Recall (top-10, 5K docs) | **100%** |
| Search latency | **0.015 ms** (SIMD) |
| Insert throughput (batch) | **4,449 vecs/s** |
| PQ compression | **32×** |

Automatic SIMD dispatch: AVX-512F → AVX2 → SSE2 (x86-64) or NEON (ARM).

---

## 8. Backup & Restore

### 8.1 Create backup

```bash
# Full backup
qm --data-dir ./mydb backup -o backup.qmvb

# With compression (lz4 fast, zstd smaller)
qm --data-dir ./mydb backup -o backup.qmvb --compress zstd

# Backup specific tables only
qm --data-dir ./mydb backup -o users.qmvb --tables users,orders

# Backup + WAL for point-in-time recovery
qm --data-dir ./mydb backup -o backup.qmvb --pitr
```

### 8.2 Restore

```bash
# Full restore
qm --data-dir ./mydb restore -i backup.qmvb

# Drop existing tables before restoring
qm --data-dir ./mydb restore -i backup.qmvb --drop-existing

# Selective restore
qm --data-dir ./mydb restore -i backup.qmvb --tables users
```

### 8.3 Differential backup (changes only)

```bash
# First time: full backup
qm --data-dir ./mydb backup -o full.qmvb

# Subsequent: only changes since full backup
qm --data-dir ./mydb diff-backup -b full.qmvb -o diff_01.qmdiff --compress lz4

# Restore: full + diff
qm --data-dir ./mydb restore -i full.qmvb
qm --data-dir ./mydb restore -i diff_01.qmdiff
```

### 8.4 Encrypt backup (AES-256-GCM)

```bash
# Encrypt
qm encrypt backup.qmvb -p 'my_secret_key'
# → backup.qmvb.enc

# Decrypt
qm decrypt backup.qmvb.enc -p 'my_secret_key' -o backup.qmvb

# Or use environment variable
export QM_ENCRYPT_KEY='my_secret_key'
qm encrypt backup.qmvb
qm decrypt backup.qmvb.enc -o backup.qmvb
```

### 8.5 Verify & estimate

```bash
# Verify integrity
qm verify backup.qmvb

# Show detailed metadata
qm verify backup.qmvb --info

# Estimate size before backup
qm --data-dir ./mydb predict --compress lz4 --json
```

### 8.6 Export data

```bash
# CSV
qm --data-dir ./mydb dump -f csv -t users -o users.csv

# JSON Lines
qm --data-dir ./mydb dump -f jsonl -t products -o products.jsonl

# SQL
qm --data-dir ./mydb dump -f sql -t users --stdout

# Parquet
qm --data-dir ./mydb dump -f parquet -t events -o events.parquet
```

### 8.7 Checkpoint (on-disk snapshot)

`checkpoint` flushes in-memory table state and search/vector indexes to the data directory and truncates the WAL. Use before filesystem-level copies or after bulk loads.

```bash
# Force checkpoint (QMvir-exclusive CLI — not available in psql)
qm --data-dir ./mydb checkpoint
```

**When to use:**

| Goal | Command |
|------|---------|
| Portable offline copy | `qm backup -o file.qmvb` (recommended) |
| Fast local durability flush | `qm checkpoint` |
| Upgrade engine, keep data dir | Stop server → upgrade binary → `qm start` (data dir unchanged) |
| Upgrade engine, portable file | `qm backup` on old version → `qm restore` on new version |

### 8.8 `.qmvb` format compatibility (v1)

QMvir **6.0+** writes `.qmvb` / `.qmdiff` with **format version 1**:

| Property | Detail |
|----------|--------|
| Magic | `QMVB` (64-byte header + JSON manifest + compressed row chunks + CRC32/HMAC footer) |
| Column types | Stable manifest tokens: `INTEGER`, `TEXT`, `VECTOR:128`, … (legacy `Debug` strings still readable) |
| Row payload | Bincode-serialized cells (vectors, JSON, BYTEA preserved bit-exact) |
| Forward compat | Backups from **6.0.x** remain restorable on **future 6.x** engines that support v1 |
| Newer file on old engine | `qm restore` fails with a clear message: *upgrade QMvir to restore this file* |

**Recommended upgrade workflow:**

```bash
# 1. On running 6.0.x
qm --data-dir ./prod backup -o prod_v6.qmvb --compress zstd
qm verify prod_v6.qmvb --info

# 2. Install new engine (6.1+, npm/pip/binary)
qm --data-dir ./prod_new restore -i prod_v6.qmvb --drop-existing

# 3. Smoke test
qm --data-dir ./prod_new sql "SELECT COUNT(*) FROM users"
```

### 8.9 Import PostgreSQL `pg_dump` (QMvir-exclusive)

`restore` auto-detects `.sql` / `.pgsql` files and imports `CREATE TABLE`, `COPY`, and `INSERT` (including `VECTOR` columns):

```bash
pg_dump -Fc is not supported — use plain SQL:
pg_dump -Fp mydb > mydb.sql
qm --data-dir ./mydb restore -i mydb.sql --drop-existing
```

### 8.10 QMvir-exclusive CLI commands (not in PostgreSQL)

These commands exist only in the `qm` binary (not via `psql`):

| Command | Purpose |
|---------|---------|
| `qm backup` | Native `.qmvb` logical backup |
| `qm restore` | Restore `.qmvb`, `.qmdiff`, or `pg_dump` SQL |
| `qm diff-backup` | LSN-based differential backup |
| `qm verify` | CRC32/HMAC integrity check |
| `qm predict` | Dry-run size/duration estimate |
| `qm encrypt` / `qm decrypt` | AES-256-GCM at-rest encryption |
| `qm checkpoint` | Flush tables + indexes, truncate WAL |
| `qm dump` | Export table to csv/jsonl/sql/parquet |
| `qm schema export` | DDL export |
| `qm schema diff` | Compare two data directories |
| `qm schema migrate` | Apply migration SQL |
| `qm benchtest` | Built-in benchmark suite |
| `qm inspect` / `qm stat` / `qm check` | Engine introspection |

Global flags: `--data-dir` (all data commands), `--lang en|vi|zht|zh`.

---

## 9. Web Dashboard & REST API

### 9.1 Start web dashboard

```bash
# Build web binary
cargo build --release --no-default-features --bin qm_web

# Start
qm_web --data-dir ./mydb --host 127.0.0.1 --port 8080 --admin-password mypassword
```

Open browser: `http://127.0.0.1:8080`

### 9.2 REST API

All endpoints require **Basic Auth**: `Authorization: Basic base64(admin:password)`

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/api/health` | Server status (version, uptime) |
| GET | `/api/stats` | Engine statistics (queries, cache, WAL) |
| GET | `/api/tables` | List tables |
| GET | `/api/tables/{name}` | Table details + sample rows |
| GET | `/api/wal/status` | WAL status |
| GET | `/metrics` | Prometheus metrics |
| POST | `/api/query` | Run SQL query |
| POST | `/api/backup` | Create backup |

### 9.3 curl examples

```bash
# Health check
curl -u admin:mypassword http://127.0.0.1:8080/api/health

# List tables
curl -u admin:mypassword http://127.0.0.1:8080/api/tables

# Run SQL
curl -X POST http://127.0.0.1:8080/api/query \
    -u admin:mypassword \
    -H "Content-Type: application/json" \
    -d '{"sql": "SELECT * FROM users LIMIT 5"}'

# Detailed statistics
curl -u admin:mypassword http://127.0.0.1:8080/api/stats

# Create backup via API
curl -X POST http://127.0.0.1:8080/api/backup \
    -u admin:mypassword \
    -H "Content-Type: application/json" \
    -d '{"output": "backup.qmvb", "compression": "lz4"}'

# Prometheus metrics
curl -u admin:mypassword http://127.0.0.1:8080/metrics
```

---

## 10. JavaScript / TypeScript SDK

### 10.1 Installation

```bash
npm install qmvir
```

### 10.2 Connect

```typescript
import { QMClient } from "qmvir";

const qm = new QMClient("http://localhost:8080", {
    apiKey: "your-api-key",       // optional
    tenantId: "tenant-001",       // optional — multi-tenant
});
```

### 10.3 CRUD

```typescript
// Search
const users = await qm.find("users", {
    where: { status: "active", age: { $gte: 18 } },
    select: ["id", "name", "email"],
    orderBy: [{ created_at: "desc" }],
    limit: 20,
    offset: 0,
});
// users.data = [{id: 1, name: "Alice", ...}, ...]

// Get 1 record
const user = await qm.get("users", "user-123");

// Insert
const result = await qm.insert("users", {
    name: "Charlie",
    email: "charlie@example.com",
    balance: 100.0,
});

// Update
await qm.update("users", "user-123", { balance: 200.0 });

// Delete
await qm.delete("users", "user-456");
```

### 10.4 Full-text Search

```typescript
const results = await qm.search("articles", "machine learning", {
    strategy: "hybrid",   // "lexical" | "vector" | "hybrid" | "rerank"
    limit: 10,
    filters: { published: true },
});
// results.data = [{id: 5, title: "...", score: 0.95}, ...]
```

### 10.5 Analytics

```typescript
const stats = await qm.aggregate("orders", {
    groupBy: ["region", "category"],
    metrics: [
        { total: "SUM(amount)" },
        { count: "COUNT(*)" },
        { avg_price: "AVG(price)" },
    ],
    where: { year: 2026 },
});
// stats.data = [{region: "US", category: "electronics", total: 54321, count: 150}, ...]
```

### 10.6 Error handling

```typescript
const result = await qm.find("nonexistent_table");
if (!result.ok) {
    console.error(result.error);  // "Table not found: nonexistent_table"
}
```

---

## 11. Advanced Configuration

### 11.1 Environment variables

| Variable | Description | Default |
|----------|-------------|----------|
| `QM_ADMIN_PASSWORD` | Admin password for server/web | (required for remote) |
| `QM_ENCRYPT_KEY` | AES-256-GCM encryption key for backup | (optional) |
| `QM_SNAPSHOT_HMAC_KEY` | HMAC key for signing backups | (optional) |
| `QM_AUDIT_LEVEL` | Audit log level | `info` |
| `QM_CACHE_SIZE_MB` | Total cache size | (auto) |
| `QM_PAGE_CACHE_MB` | Page cache size | (auto) |
| `QM_QUERY_CACHE_MB` | Query result cache size | (auto) |

### 11.2 WAL (Write-Ahead Log)

WAL ensures crash recovery — all changes are written to WAL before being applied.

```bash
# Force checkpoint (flush WAL → storage)
qm --data-dir ./mydb checkpoint
```

**Group Commit** (6.0): engine-native `group_commit_sync` batches fsync across concurrent writers. Set via Python API:

```python
engine.set_wal_sync_policy("group_commit_sync")  # alias: "group_commit"
```

Policies: `per_commit_sync`, `group_commit_sync`, `relaxed_os_buffered`. See the performance guide §8 for benchmark scripts (`compare_postgres_group_commit.py`, `native_sql_write_profile.py`).

**io_uring** (Linux 5.1+): Zero-syscall I/O via submission queue. Automatic fallback to `pwrite64` if kernel does not support it.

Check if io_uring is active:
```bash
curl -u admin:pw http://127.0.0.1:8080/api/wal/status
```

### 11.3 Cache (W-TinyLFU)

3-tier scan-resistant cache:

| Tier | Ratio | Role |
|------|-------|------|
| Window | 1% | LRU — receives all new entries |
| Protected | 80% | Frequently accessed entries |
| Probationary | 19% | Entries about to be evicted |

Sequential scans do not push hot data out of cache.

### 11.4 Mmap & madvise

For data larger than RAM, QM uses `mmap` + `madvise` hints:

- `MADV_SEQUENTIAL` — sequential reads (bulk scan)
- `MADV_RANDOM` — random reads (point lookup)
- `MADV_WILLNEED` — prefetch before needed

Result: prefetch reads **14.6×** faster.

### 11.5 Unix Socket

```bash
# Connect via unix socket (faster than TCP on same machine)
qm --data-dir ./mydb start --unix-socket /tmp/qm.sock

# Connect psql via socket
psql -h /tmp -p 55433 -U admin -d qm
```

---

## 12. Statistics & Monitoring

### 12.1 CLI

```bash
# Overview statistics
qm --data-dir ./mydb stat
qm --data-dir ./mydb stat --json

# List tables
qm --data-dir ./mydb inspect --tables

# Table details (columns, row count, size)
qm --data-dir ./mydb inspect --table users

# Integrity check
qm --data-dir ./mydb check --all
qm --data-dir ./mydb check --table users
```

### 12.2 Prometheus Metrics

```bash
# Endpoint
curl -u admin:pw http://127.0.0.1:8080/metrics

# Example output
qm_queries_total{type="select"} 15234
qm_queries_total{type="insert"} 8921
qm_cache_hits_total 123456
qm_cache_misses_total 789
qm_wal_writes_total 45678
qm_wal_size_bytes 104857600
qm_connections_active 12
qm_connections_max 1000
```

Can be integrated with Grafana / Victoria Metrics / any Prometheus scraper.

### 12.3 Built-in statistical sketches

| Sketch | Function | Error | Memory |
|--------|----------|-------|--------|
| **HyperLogLog** | Estimate COUNT DISTINCT | ~0.81% | 16 KB |
| **Count-Min Sketch** | Estimate frequency | ~0.13% | few KB |
| **T-Digest** | Estimate percentiles (P50, P99) | <1% at tail | ~5 KB |
| **Bloom Filter** | Check "does it exist?" | tunable FP rate | O(n) bits |

---

## 13. Cluster & Sharding

### 13.1 Consistent Hash Ring

QM includes deterministic shard routing and local sharded index behavior using consistent hashing. Distributed coordination, replication, and cross-shard transaction behavior are experimental unless validated by integrated failure/recovery tests for your deployment.

```sql
-- Create sharded index for large datasets
CREATE SHARDED INDEX idx_vec ON huge_table (embedding)
    WITH (num_shards=16, replica_factor=3);
```

Supported local sharded-index queries are routed deterministically. Do not treat this as a production HA or network-partition safety claim.

### 13.2 Replication

Replication modes are experimental release-surface documentation, not a production HA guarantee.

| Mode | Description |
|------|-------------|
| **Async** | Leader writes first, replica catches up later |
| **HalfSync** | Wait for at least 1 replica to acknowledge |
| **FullSync** | Wait for all replicas to acknowledge |

### 13.3 WAL Streaming

Leader → Replica via WAL streaming (sender/receiver). Automatic recovery on restart.

---

## 14. Security

### 14.1 Authentication

- **SCRAM-SHA-256** (preferred) — PostgreSQL standard
- MD5 fallback
- Cleartext fallback (only on loopback)

```bash
# Require password for remote connections
qm start --host 0.0.0.0 --port 55433 --admin-password strong_password_here
```

When no password is set and binding to non-loopback → server refuses to start.

### 14.2 Backup encryption

AES-256-GCM (authenticated encryption):

```bash
export QM_ENCRYPT_KEY='32-byte-key-here'
qm backup -o backup.qmvb --compress zstd
qm encrypt backup.qmvb
# → backup.qmvb.enc (encrypted + integrity verified)
```

### 14.3 HMAC Signing

```bash
export QM_SNAPSHOT_HMAC_KEY='signing-key'
qm backup -o backup.qmvb
# Each chunk in the backup is signed with HMAC-SHA256
# Automatically verified on restore
```

### 14.4 Web Dashboard

- Only accepts loopback connections (127.0.0.1, ::1)
- HTTP Basic Auth required
- Use a reverse proxy (nginx/caddy) to expose externally

---

## 15. Benchmark

### 15.1 Quick benchmark

```bash
# Run all 17 built-in benchmarks (quick profile, ~10 seconds)
qm benchtest

# Standard profile (larger datasets, more iterations)
qm benchtest --profile standard

# JSON output for CI/CD integration
qm benchtest --json
```

### 15.2 Benchmark coverage

The `benchtest` command exercises 17 core components:

| # | Benchmark | What it measures |
|---|-----------|-----------------|
| 1 | Ring Buffer IPC | Lock-free inter-thread message passing |
| 2 | LSN Sequencer | Atomic WAL sequence number generation |
| 3 | W-TinyLFU Cache | Scan-resistant cache hit/miss performance |
| 4 | B+Tree Insert | Ordered index insertion throughput |
| 5 | B+Tree Lookup | Point query on B+Tree index |
| 6 | Roaring Bitmap | Compressed bitmap set operations |
| 7 | JIT batch_filter | JIT-compiled predicate evaluation |
| 8 | HNSW Build | Vector index construction |
| 9 | HNSW Search | Approximate nearest neighbor query |
| 10 | HyperLogLog | Cardinality estimation accuracy |
| 11 | Bloom Filter | Probabilistic membership test |
| 12 | SQL INSERT | End-to-end row insertion |
| 13 | SQL SELECT | End-to-end row retrieval |
| 14 | SQL COUNT | Aggregation query performance |
| 15 | B+Tree Range | Range scan on ordered index |
| 16 | Bitmap AND/OR | Set intersection/union operations |
| 17 | Full Pipeline | INSERT → SELECT → COUNT combined |

### 15.3 Example output

```
$ qm benchtest
╔══════════════════════════════════════════════════════════════╗
║                    QMvir Benchmark Suite                     ║
╠══════════════════════════════════════════════════════════════╣

 Profile: quick

 [1/17] Ring Buffer IPC .................. 2.1M msgs/sec    ✓
 [2/17] LSN Sequencer .................... 45.3M ops/sec    ✓
 [3/17] W-TinyLFU Cache (10K ops) ........ 8.7M ops/sec    ✓
 [4/17] B+Tree Insert (1K keys) .......... 1.2M ops/sec    ✓
 [5/17] B+Tree Lookup (1K keys) .......... 3.8M ops/sec    ✓
 ...
 [17/17] Full Pipeline ................... 42,150 rows/sec  ✓

╠══════════════════════════════════════════════════════════════╣
║  All 17 benchmarks passed                                    ║
╚══════════════════════════════════════════════════════════════╝
```

### 15.4 Segment benchmarks (vs DuckDB, Qdrant, PostgreSQL)

```bash
python scripts/run_segment_benchmark_suite.py
bash scripts/run_quizzman_docker_fair_bench.sh   # fair all-container mode
```

See [QMVIR_PERFORMANCE_GUIDE_VI.md](docs/QMVIR_PERFORMANCE_GUIDE_VI.md) for workload patterns, HNSW tuning, and WAL policies.

---

## 16. CLI Reference

### All commands

```
qm [--data-dir PATH] [--lang LANG] <COMMAND>

COMMANDS:
  start           Start pgwire server (daemon by default)
  stop            Stop server
  status          Server status
  sql <QUERY>     Run SQL directly
  backup          Create backup (.qmvb)
  restore         Restore from backup
  diff-backup     Differential backup
  verify          Verify backup integrity
  predict         Estimate backup size
  encrypt         Encrypt file (AES-256-GCM)
  decrypt         Decrypt file
  dump            Export data (csv/jsonl/sql/parquet)
  inspect         View table info
  stat            Engine statistics
  check           Integrity check
  checkpoint      Force WAL flush
  schema          Schema diff / export / migrate
  benchtest       Run built-in benchmark suite
  guide           Usage guide & important notes (alias: help)
  version         Show version
```

### Global options

```
  --data-dir <PATH>          Data directory (default: ./data)
  --lang <LANG>              CLI language: en | vi | zht | zh (default: en)
```

### start

```
qm start [OPTIONS]
  --host <ADDR>              Listen address (default: 127.0.0.1)
  --port <PORT>              TCP port (default: 55433)
  --max-connections <N>      Max connections (default: 1000)
  --unix-socket <PATH>       Unix domain socket
  --admin-password <PW>      Admin password (or QM_ADMIN_PASSWORD)
  --foreground               Run in foreground (default: daemon)
```

### benchtest

```
qm benchtest [OPTIONS]
  --profile <PROFILE>        Benchmark profile: quick | standard (default: quick)
  --json                     Output results as JSON
```

### backup

```
qm backup [OPTIONS]
  -o, --output <FILE>        Output file (.qmvb)
  --compress <ALGO>          Compression: none | lz4 | zstd
  --tables <T1,T2,...>       Only backup these tables
  --pitr                     Include WAL for point-in-time recovery
```

### guide

```
qm guide [TOPIC]           # alias: qm help
  TOPIC: all | quickstart | backup | studio | cli | notes
```

Built-in offline guide — no network required. Respects `--lang en|vi|zht|zh`.

```bash
qm guide                   # overview + essential commands
qm guide quickstart        # install, start, psql, first SQL
qm guide backup            # .qmvb, restore, encrypt, dump, upgrade path
qm guide studio            # QMvir Studio desktop + qm_web dashboard
qm guide cli               # QMvir-exclusive commands (not in psql)
qm guide notes             # caveats before production
qm --lang vi guide notes
```

### dump

```
qm dump [OPTIONS]
  -f, --format <FMT>         Format: csv | jsonl | sql | parquet (default: sql)
  -t, --table <TABLE>        Table to export (all tables if omitted)
  -o, --output <FILE>        Output file
  --stdout                   Write to stdout (sql/csv/jsonl only)
```

### restore

```
qm restore [OPTIONS]
  -i, --input <FILE>         .qmvb, .qmdiff, or pg_dump .sql/.pgsql
  --drop-existing            Drop tables before restore
  --tables <T1,T2,...>       Restore only listed tables
```

### verify

```
qm verify <FILE> [--info]    Integrity check; --info prints manifest metadata
```

### checkpoint

```
qm checkpoint                Flush data dir snapshot + truncate WAL
```

### diff-backup

```
qm diff-backup [OPTIONS]
  -b, --base <FILE>          Base .qmvb backup
  -o, --output <FILE>        Output .qmdiff
  --compress <ALGO>          none | lz4 | zstd
```

### encrypt / decrypt

```
qm encrypt <FILE> -p <PASSWORD>     Password or QM_ENCRYPT_KEY env
qm decrypt <FILE> -o <OUT> -p <PW>
```

### schema

```
qm schema export             Export CREATE TABLE DDL
qm schema diff <DIR_A> <DIR_B> [--output migration.sql]
qm schema migrate <FILE> [--dry-run]
```

---

*QM Engine v6.0.0 — Native SQL OLAP/vector/search performance release*
