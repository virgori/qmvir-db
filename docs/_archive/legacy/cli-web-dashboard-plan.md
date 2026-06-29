# QMvir CLI Tool & Web Dashboard — Implementation Plan

**Version**: 2.2.0 &nbsp;|&nbsp; **Base**: QMvir v2.0.0 engine  
**Date**: 2026-04-02  
**Strategy**: CLI-first (bulletproof) → Web Dashboard (lightweight)

---

## 0. Tổng quan kiến trúc

```
qm_engine/
├── src/
│   ├── lib.rs              ← PyO3 library (existing)
│   ├── bin/
│   │   ├── qm.rs           ← Unified CLI entry point (mới)
│   │   └── qm_web.rs       ← Web dashboard server (mới)
│   ├── cli/
│   │   ├── mod.rs           ← clap App definition
│   │   ├── inspect.rs       ← qm inspect
│   │   ├── stat.rs          ← qm stat
│   │   ├── check.rs         ← qm check
│   │   ├── backup.rs        ← qm backup / restore / verify / predict
│   │   ├── schema.rs        ← qm schema diff/migrate
│   │   └── dump.rs          ← qm dump (streaming)
│   ├── web/
│   │   ├── mod.rs           ← axum Router + state
│   │   ├── api.rs           ← REST JSON endpoints
│   │   ├── metrics.rs       ← /metrics Prometheus endpoint
│   │   └── dashboard.rs     ← embedded HTML/JS (single-page)
│   ├── backup/              ← (from previous plan, Phase 2.1)
│   ├── gateway/             ← (existing)
│   ├── storage/             ← (existing)
│   └── ...
Cargo.toml                   ← add [[bin]], new deps
```

**Triết lý thiết kế**:
- Tất cả CLI commands gọi trực tiếp vào engine Rust, không qua TCP/IPC
- Web dashboard nhúng static HTML/JS vào binary, không cần build step frontend
- `qm` binary là single multicall binary (giống `busybox`): `qm inspect`, `qm stat`, `qm backup`...

---

## 1. CLI Tool — `qm`

### 1.1 Command Map

```
qm
├── inspect    — Page/WAL/Snapshot introspection
│   ├── --page <id>         Hexdump + parsed PageHeader
│   ├── --wal [--tail N]    WAL record browser
│   ├── --snapshot <path>   Snapshot file inspector
│   └── --table <name>      Table metadata + row samples
│
├── stat       — Real-time engine statistics
│   ├── --live              Continuous dashboard (TUI refresh)
│   ├── --once              One-shot snapshot
│   ├── --json              JSON output
│   └── --prometheus        Prometheus text format
│
├── check      — Integrity verification & repair
│   ├── --all               Full integrity scan
│   ├── --table <name>      Check specific table
│   ├── --wal               WAL CRC verification
│   ├── --fix               Attempt auto-repair
│   └── --checksum-only     Quick checksum pass
│
├── backup     — Create backups (from Backup Suite plan)
│   ├── --all / --tables
│   ├── --compress lz4|zstd
│   ├── --encrypt           AES-256-GCM encryption
│   ├── --incremental       Since last backup (cumulative)
│   ├── --pitr              Include WAL for point-in-time
│   └── --output <path>
│
├── restore    — Restore from backup / PG dump
│   ├── --from <path>       .qmbk / .qmdiff / .sql
│   ├── --at <timestamp>    PITR target (ISO-8601)
│   ├── --decrypt <key>     Decrypt AES backup
│   ├── --tables t1,t2      Selective restore
│   └── --dry-run           Validate without writing
│
├── verify     — Backup verification
│   ├── --file <path>       CRC + HMAC check
│   ├── --deep              Decompress & count rows
│   └── --info              Show backup metadata
│
├── predict    — Estimation
│   ├── --backup            Estimate backup size
│   ├── --diff --since N    Estimate diff size
│   └── --json              JSON output
│
├── dump       — Streaming export
│   ├── --format sql|csv|parquet|jsonl
│   ├── --table <name>
│   ├── --stdout            Pipe-friendly (qm dump | gzip > backup.gz)
│   └── --parallel <N>      Multi-table parallel dump
│
├── schema     — Schema management
│   ├── diff <db1> <db2>    Compare two data dirs
│   ├── migrate <diff.sql>  Apply migration
│   └── export              Dump DDL only
│
└── version    — Build info
```

### 1.2 `qm inspect` — Deep Introspection

**Mục tiêu**: Debug page-level corruption, verify compression, inspect WAL records.

```
$ qm inspect --page 42 --data-dir ./data

╔══════════════════════════════════════════════════╗
║  Page #42 — Data Page                            ║
╠══════════════════════════════════════════════════╣
║  Type:        Data (0x01)                        ║
║  Flags:       0x00                               ║
║  Items:       127                                ║
║  Free space:  1,284 / 8,192 bytes (15.7%)        ║
║  Checksum:    0x3A7F_BC01                        ║
║  LSN:         0x0000_0000_0000_1A3F              ║
║  Prev/Next:   41 → 43                            ║
╠══════════════════════════════════════════════════╣
║  Hex dump (first 256 bytes):                     ║
║  0000: 2A 00 00 00 01 00 00 00  40 00 00 00 08 20 7F 00  ║
║  0010: 01 BC 7F 3A 3F 1A 00 00  00 00 00 00 00 00 29 00  ║
║  ...                                                      ║
╚══════════════════════════════════════════════════╝
```

**Implementation** (`cli/inspect.rs`):

```rust
use crate::storage::page::{PageHeader, PageType, Page};
use crate::storage::wal::WalRecord;
use crate::storage::snapshot::{SnapshotHeader, SnapshotReader};

pub fn inspect_page(data_dir: &Path, page_id: u32) -> Result<()> {
    // 1. Memory-map the page file
    // 2. Read PageHeader::from_bytes at offset page_id * page_size
    // 3. Compute checksum, compare with stored
    // 4. Pretty-print header fields + hex dump
}

pub fn inspect_wal(data_dir: &Path, tail: Option<usize>) -> Result<()> {
    // 1. Read WAL files in order
    // 2. Decode WalRecord per record
    // 3. Verify CRC per record, flag mismatches
    // 4. Display: LSN | Type | TxnID | Size | CRC_OK
}

pub fn inspect_snapshot(path: &Path) -> Result<()> {
    // 1. SnapshotReader::read_snapshot() — already verifies CRC + HMAC
    // 2. Print SnapshotHeader: magic, version, LSN, page_count, timestamp
    // 3. Summary: total pages, compressed/original ratio
}

pub fn inspect_table(data_dir: &Path, table: &str) -> Result<()> {
    // 1. Load NativeSqlEngine from data_dir
    // 2. tables.read().get(table) → columns, types, row_count
    // 3. Sample first 5 rows
}
```

**Infrastructure reuse**:
- `PageHeader::from_bytes()` — already exists, [page.rs](page.rs#L35)
- `WalRecord::decode()` — already exists, [wal.rs](wal.rs#L66)
- `SnapshotReader::read_snapshot()` — already exists, [snapshot.rs](snapshot.rs#L305)

**New code**: ~200 LOC

---

### 1.3 `qm stat` — Real-time Statistics Dashboard

**Mục tiêu**: Live monitoring dashboard in terminal.

```
$ qm stat --live --data-dir ./data

QMvir v2.0.0 — Live Statistics (refresh: 1s)     [Ctrl+C to exit]
────────────────────────────────────────────────────────────────
 Queries       │  1,234/s     total: 45,678
 Inserts       │    567/s     total: 12,345
 Deletes       │     23/s     total:    891
────────────────────────────────────────────────────────────────
 Cache Hit Rate│ ████████████████░░░░  82.3%
 Buffer Pool   │  98.2 MB / 128 MB
 Active TXN    │  5
────────────────────────────────────────────────────────────────
 WAL Size      │  24.3 MB  (384 records since checkpoint)
 WAL Mutations │  3,847 / 10,000 until auto-checkpoint
 Last LSN      │  0x0000_0000_0000_2F1A
────────────────────────────────────────────────────────────────
 Tables: 12    │  Indexes: 8 (3 active, 2 building, 3 manual)
 Total Rows    │  1,234,567
────────────────────────────────────────────────────────────────
 Latency p50   │  0.8ms    p99: 12.3ms    max: 45.1ms
```

**Implementation** (`cli/stat.rs`):

```rust
use crate::metrics::MetricsRegistry;
use crate::gateway::native_sql::NativeSqlEngine;

pub fn stat_live(engine: &NativeSqlEngine, metrics: &MetricsRegistry) -> Result<()> {
    // Terminal raw mode for refresh
    loop {
        // 1. Read all counters/gauges/histograms from MetricsRegistry
        // 2. Compute deltas (ops/s) from previous snapshot
        // 3. Read WAL file size, mutation count
        // 4. Read table count, total rows from engine.tables.read()
        // 5. Render TUI table with indicatif/crossterm
        // 6. Sleep 1s, clear screen, repeat
    }
}

pub fn stat_once(engine: &NativeSqlEngine, metrics: &MetricsRegistry, 
                 json: bool) -> Result<()> {
    // Same data collection, single render
    // If json: serde_json::to_string_pretty()
    // If prometheus: metrics.render()
}
```

**Infrastructure reuse**:
- `MetricsRegistry` — 9 counters + 5 gauges + 3 histograms, all lock-free
- `MetricsRegistry::render()` — Prometheus text format
- `wal_mutations.load()` — mutation count since checkpoint
- `tables.read().len()` — table count
- `IndexManager::list_indexes()` — index diagnostics

**New code**: ~180 LOC

---

### 1.4 `qm check` — Integrity Verification & Repair

**Mục tiêu**: Scan for corruption, orphaned data, checksum mismatches.

```
$ qm check --all --data-dir ./data

Checking snapshot integrity...
  ✓ native_sql.snap: bincode valid, 12 tables, 1,234,567 rows
Checking WAL integrity...
  ✓ native_sql.wal: 384 records, 0 CRC errors
Checking table consistency...
  ✓ users: 45,678 rows, all columns present
  ✓ orders: 123,456 rows, all columns present
  ⚠ products: 2 rows with NULL in non-nullable column "price"
Checking index consistency...
  ✓ idx_users_email: 45,678 entries, matches table
  ⚠ idx_orders_date: 123,400 entries, 56 orphaned (table has 123,456)
Checking page checksums...
  ✓ 1,024 pages verified, 0 checksum errors

Summary: 2 warnings, 0 errors
```

```
$ qm check --fix --data-dir ./data

  Fixed: idx_orders_date — rebuilt index (56 orphaned entries removed)
  Fixed: products — set 2 NULL prices to 0.0 (default)
```

**Implementation** (`cli/check.rs`):

```rust
pub struct CheckEngine<'a> {
    engine: &'a NativeSqlEngine,
    data_dir: &'a Path,
}

pub struct CheckResult {
    pub snapshot_ok: bool,
    pub wal_errors: Vec<WalError>,
    pub table_issues: Vec<TableIssue>,
    pub index_issues: Vec<IndexIssue>,
    pub page_errors: Vec<PageError>,
}

impl<'a> CheckEngine<'a> {
    /// Full integrity scan.
    pub fn check_all(&self) -> CheckResult;
    
    /// Verify snapshot file is valid bincode.
    pub fn check_snapshot(&self) -> Result<SnapshotCheck>;
    
    /// Verify every WAL record CRC.
    pub fn check_wal(&self) -> Result<Vec<WalError>>;
    
    /// Cross-check table rows vs index entries.
    pub fn check_table_index_consistency(&self, table: &str) -> Result<Vec<IndexIssue>>;
    
    /// Verify page checksums in page file.
    pub fn check_pages(&self) -> Result<Vec<PageError>>;
    
    /// Attempt repair for known issue types.
    pub fn fix(&self, issues: &CheckResult) -> Vec<FixAction>;
}
```

**Repair capabilities** (conservative — only safe fixes):
| Issue | Fix |
|---|---|
| Orphaned index entries | Rebuild index from table |
| WAL CRC mismatch at tail | Truncate to last valid record |
| Stale columnar cache | Delete `spill/*.colcache`, rebuild |
| Missing snapshot | Force checkpoint from memory |

**Infrastructure reuse**:
- `bincode::deserialize` — validate snapshot
- WAL CRC verify — existing logic in `replay_wal()` L750
- `IndexManager::list_indexes()` + cross-check with tables
- `PageHeader::from_bytes()` + checksum verification

**New code**: ~280 LOC

---

### 1.5 `qm backup` / `qm restore` / `qm verify` / `qm predict`

Đã lên kế hoạch chi tiết trong [backup-migration-suite-plan.md](backup-migration-suite-plan.md).
CLI wrappers sẽ delegate sang `backup/` module.

**Bổ sung so với plan trước**:

#### A. Incremental Backup (Lũy tiến)

Khác với Differential (so với Full), Incremental chỉ backup phần thay đổi so với backup *gần nhất*.

```
Full (LSN 0-1000)
  └── Incr #1 (LSN 1001-1500)
       └── Incr #2 (LSN 1501-1800)
            └── Incr #3 (LSN 1801-2000)
```

**Implementation**: Thêm `backup_type: Incremental(3)` vào `.qmbk` header.
Mỗi backup ghi `end_lsn` vào manifest. Backup kế tiếp dùng `end_lsn` làm `base_lsn`.

Restore: phải apply chain Full → Incr#1 → Incr#2 → Incr#3 theo thứ tự.

```rust
// backup/backup.rs
pub fn create_incremental(&self, last_backup: &Path) -> io::Result<BackupResult> {
    // 1. Read last_backup header → extract end_lsn
    // 2. Collect rows where last_modified_lsn > end_lsn
    // 3. Write .qmbk with backup_type = Incremental
}

// backup/restore.rs
pub fn restore_chain(&self, backups: &[PathBuf]) -> io::Result<RestoreResult> {
    // 1. Verify chain: Full → Incr → Incr → ... (LSN continuity)
    // 2. Apply each in order
}
```

**New code**: ~100 LOC thêm vào backup module

#### B. Point-in-Time Recovery (PITR)

Restore snapshot + replay WAL đến một thời điểm cụ thể.

```
$ qm restore --from backup.qmbk --at "2026-04-01T14:30:00Z" --data-dir ./data_restore
```

**Implementation**: 
1. Restore `.qmbk` snapshot (full state at backup time)
2. Extract WAL segment from `.qmbk` (nếu `--pitr` flag lúc backup)
3. Replay WAL records, **dừng lại** khi record timestamp > target time

```rust
pub fn restore_pitr(&self, backup: &Path, target: DateTime<Utc>) -> io::Result<RestoreResult> {
    // 1. Restore base snapshot
    // 2. Extract WAL from backup
    // 3. For each WalRecord: if timestamp(record) > target → stop
    // 4. checkpoint() to finalize
}
```

**Yêu cầu**: WAL records hiện tại KHÔNG có timestamp — cần thêm `timestamp: u64` vào `WalRecord`.

**Schema change** (WAL format v2):
```rust
// storage/wal.rs
pub struct WalRecord {
    pub lsn: u64,
    pub record_type: WalRecordType,
    pub txn_id: u64,
    pub timestamp: u64,   // NEW — unix epoch secs
    pub data: Vec<u8>,
}
```

Binary format: `[LSN:8][Type:1][TxnID:8][Timestamp:8][Length:4][Data:*][CRC32:4]`

**New code**: ~80 LOC

#### C. Encryption at Rest (AES-256-GCM)

```
$ qm backup --all --encrypt --output backup.qmbk.enc
Enter encryption key: ********

$ qm restore --from backup.qmbk.enc --decrypt
Enter decryption key: ********
```

**Implementation**: Wrap compressed `.qmbk` content in AES-256-GCM envelope.

```
┌─────────────────────────────────────────┐
│ Encryption Header (32 bytes)            │
│   magic:       u64 = 0x514D_454E_4300   │
│   version:     u32 = 1                  │
│   kdf:         u8  = Argon2id(1)        │
│   nonce:       [u8; 12]  (random)       │
│   salt:        [u8; 16]  (for KDF)      │
├─────────────────────────────────────────┤
│ Encrypted payload                       │
│   AES-256-GCM(key, nonce, .qmbk bytes)  │
├─────────────────────────────────────────┤
│ Auth tag:      [u8; 16]                 │
└─────────────────────────────────────────┘
```

**Key derivation**: Argon2id (đã có crate `argon2 = "0.5"` trong Cargo.toml)

**New Cargo dep**: `aes-gcm = "0.10"` (authenticated encryption)

**New code**: ~120 LOC (`backup/encrypt.rs`)

---

### 1.6 `qm dump` — Streaming Export

```
$ qm dump --table users --format csv --stdout | gzip > users.csv.gz
$ qm dump --format sql --stdout | ssh prod-server "qm restore --from -"
$ qm dump --format jsonl --table orders --stdout | jq '.total > 100'
```

**Implementation** (`cli/dump.rs`):

```rust
pub enum DumpFormat { Sql, Csv, Parquet, Jsonl }

pub fn dump_table(
    engine: &NativeSqlEngine,
    table: &str,
    format: DumpFormat,
    writer: &mut dyn Write,  // stdout or file
) -> Result<u64> {
    let tables = engine.tables.read();
    let t = tables.get(table).ok_or("table not found")?;
    match format {
        DumpFormat::Sql => dump_as_sql(t, table, writer),
        DumpFormat::Csv => dump_as_csv(t, writer),
        DumpFormat::Jsonl => dump_as_jsonl(t, writer),
        DumpFormat::Parquet => dump_as_parquet(t, table, writer),
    }
}
```

**Infrastructure reuse**:
- `copy_to_parquet()` — existing Parquet export logic
- `NativeTable` serialization — same cell formatting

**New code**: ~150 LOC

---

### 1.7 `qm schema` — Schema Diff & Migration

```
$ qm schema diff ./data_prod ./data_staging

--- prod
+++ staging

Table "users":
  + ADD COLUMN avatar TEXT
  - DROP COLUMN legacy_id

Table "orders":
  (identical)

New table in staging:
  + CREATE TABLE analytics (id INT, event TEXT, ts TEXT)

Missing from staging:
  - DROP TABLE temp_imports

$ qm schema diff ./data_prod ./data_staging --output migrate.sql

Generated: migrate.sql (4 statements)

$ qm schema migrate migrate.sql --data-dir ./data_prod --dry-run
  ALTER TABLE users ADD COLUMN avatar TEXT     → OK (simulated)
  ALTER TABLE users DROP COLUMN legacy_id      → OK (simulated)
  CREATE TABLE analytics (id INT, event TEXT, ts TEXT)  → OK (simulated)
  DROP TABLE temp_imports                       → OK (simulated)
```

**Implementation** (`cli/schema.rs`):

```rust
pub struct SchemaDiff {
    pub added_tables: Vec<String>,
    pub removed_tables: Vec<String>,
    pub modified_tables: Vec<TableDiff>,
}

pub struct TableDiff {
    pub table: String,
    pub added_columns: Vec<(String, ColType)>,
    pub removed_columns: Vec<String>,
    pub type_changes: Vec<(String, ColType, ColType)>,  // col, old, new
}

pub fn diff_schemas(dir_a: &Path, dir_b: &Path) -> Result<SchemaDiff> {
    // 1. Load both NativeSqlEngines (read-only)
    // 2. Compare table sets
    // 3. For matching tables: compare columns & types
}

pub fn generate_migration_sql(diff: &SchemaDiff) -> String {
    // Emit ALTER TABLE ADD/DROP COLUMN, CREATE/DROP TABLE
}
```

**New code**: ~200 LOC

---

## 2. Web Dashboard — `qm-web`

### 2.1 Tại sao Web, không phải GUI Native

| Tiêu chí | CLI | Web Dashboard | Native GUI |
|---|---|---|---|
| Dev cost | Thấp | Trung bình | Cao |
| Cross-platform | ✅ | ✅ (browser) | ❌ (per-platform) |
| Charts/visualization | ❌ | ✅ (Chart.js) | 🟡 |
| Maintenance | Thấp | Thấp | Cao (40%+ time) |
| Target audience | DBA/DevOps | Everyone | End users |

**Kết luận**: Embedded HTTP server + single-page HTML dashboard.

### 2.2 Architecture

```
qm-web (axum server)
├── GET  /                    → Dashboard HTML (embedded)
├── GET  /api/stats           → JSON: counters, gauges, histograms
├── GET  /api/tables          → JSON: table list + metadata
├── GET  /api/tables/:name    → JSON: table schema + row count + samples
├── GET  /api/indexes         → JSON: index list + stats
├── GET  /api/wal/status      → JSON: WAL size, mutation count, LSN
├── GET  /api/health          → JSON: { status: "ok", uptime, version }
├── GET  /metrics             → Prometheus text format (existing render())
├── POST /api/query           → JSON: { sql: "SELECT ..." } → result rows
├── POST /api/backup          → Trigger backup, return result
└── POST /api/checkpoint      → Trigger manual checkpoint
```

### 2.3 Backend Implementation (`web/`)

```rust
// web/mod.rs
use axum::{Router, routing::{get, post}, Json, extract::State};
use std::sync::Arc;
use tokio::net::TcpListener;

pub struct WebState {
    engine: Arc<NativeSqlEngine>,
    metrics: Arc<MetricsRegistry>,
    start_time: Instant,
}

pub fn router(state: Arc<WebState>) -> Router {
    Router::new()
        .route("/", get(dashboard_html))
        .route("/api/stats", get(api_stats))
        .route("/api/tables", get(api_tables))
        .route("/api/tables/:name", get(api_table_detail))
        .route("/api/indexes", get(api_indexes))
        .route("/api/wal/status", get(api_wal_status))
        .route("/api/health", get(api_health))
        .route("/metrics", get(api_metrics))
        .route("/api/query", post(api_query))
        .route("/api/backup", post(api_backup))
        .route("/api/checkpoint", post(api_checkpoint))
        .with_state(state)
}

pub async fn start_web(state: Arc<WebState>, addr: &str) -> Result<()> {
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}
```

```rust
// web/api.rs
async fn api_stats(State(s): State<Arc<WebState>>) -> Json<StatsResponse> {
    let m = &s.metrics;
    Json(StatsResponse {
        queries_total: m.queries_total.get(),
        inserts_total: m.inserts_total.get(),
        cache_hit_rate: cache_hit_rate(m),
        active_txns: m.active_txns.get() as u64,
        wal_size: m.wal_size_bytes.get() as u64,
        uptime_secs: s.start_time.elapsed().as_secs(),
        // ... all 17 metrics
    })
}

async fn api_tables(State(s): State<Arc<WebState>>) -> Json<Vec<TableInfo>> {
    let tables = s.engine.tables.read();
    let infos: Vec<_> = tables.iter().map(|(name, t)| TableInfo {
        name: name.clone(),
        columns: t.columns.clone(),
        column_types: t.column_types.iter().map(|c| format!("{:?}", c)).collect(),
        row_count: t.rows.len(),
    }).collect();
    Json(infos)
}

async fn api_query(
    State(s): State<Arc<WebState>>,
    Json(req): Json<QueryRequest>,
) -> Result<Json<QueryResponse>, StatusCode> {
    // Input validation — prevent injection into internal APIs
    if req.sql.len() > 10_000 { return Err(StatusCode::BAD_REQUEST); }
    let result = s.engine.execute(&req.sql);
    // ... serialize to JSON
}
```

**Security**:
- `api_query` gated behind auth (reuse existing `AuthManager`)
- Rate limiting via `tower::limit::RateLimitLayer`
- Write endpoints (`backup`, `checkpoint`, `query` with DML) require admin auth
- CORS headers locked to `localhost` by default

### 2.4 Frontend — Embedded Single-Page Dashboard

Nhúng HTML/CSS/JS trực tiếp vào binary bằng `include_str!()`.

```rust
// web/dashboard.rs
const DASHBOARD_HTML: &str = include_str!("../../static/dashboard.html");

async fn dashboard_html() -> Html<&'static str> {
    Html(DASHBOARD_HTML)
}
```

**Dashboard layout** (`static/dashboard.html`):

```
┌─────────────────────────────────────────────────────────────┐
│  QMvir Dashboard v2.2.0           [localhost:8080]   🟢 UP  │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐       │
│  │ 45,678   │ │  82.3%   │ │   5      │ │  24.3MB  │       │
│  │ Queries  │ │ Hit Rate │ │ Active   │ │ WAL Size │       │
│  └──────────┘ └──────────┘ └──────────┘ └──────────┘       │
│                                                             │
│  ┌─── Throughput (last 60s) ────────────────────────┐       │
│  │  ▁▂▃▅▇█▇▅▃▂▁▂▃▅▇█▇▅▃▂▁▂▃▅▇█▇▅▃                │       │
│  │  1,200 qps ────────────────────▶                 │       │
│  └──────────────────────────────────────────────────┘       │
│                                                             │
│  ┌─── Tables ───────────────────────────────────────┐       │
│  │  Name      │ Columns │  Rows    │ Indexes        │       │
│  │  users     │   5     │  45,678  │ 2 (active)     │       │
│  │  orders    │   8     │ 123,456  │ 3 (1 building) │       │
│  │  products  │   4     │   2,345  │ 1              │       │
│  └──────────────────────────────────────────────────┘       │
│                                                             │
│  ┌─── SQL Console ──────────────────────────────────┐       │
│  │  SELECT * FROM users WHERE id = 42               │       │
│  │  [Execute]                                       │       │
│  │  Result: { id: 42, name: "Alice", ... }          │       │
│  └──────────────────────────────────────────────────┘       │
│                                                             │
│  ┌─── Latency Distribution ─────────────────────────┐       │
│  │  p50: 0.8ms  p90: 5.2ms  p99: 12.3ms  max: 45ms │       │
│  │  ▂▅█▇▅▃▂▁                                       │       │
│  └──────────────────────────────────────────────────┘       │
└─────────────────────────────────────────────────────────────┘
```

**JS libraries** (CDN, no build step):
- **Chart.js 4.x** — throughput chart, latency histogram
- **Vanilla JS** — no React/Vue overhead, <10KB JS total

**Auto-refresh**: `setInterval(() => fetch('/api/stats'), 1000)` → update DOM.

### 2.5 Web Dashboard Static File (~300 LOC HTML/CSS/JS)

```
qm_engine/
└── static/
    └── dashboard.html    ← Single file, embedded at compile time
```

---

## 3. Dependency Changes

### Cargo.toml additions:

```toml
[dependencies]
# CLI framework
clap = { version = "4", features = ["derive", "color", "suggestions"] }
# Progress bars & terminal UI
indicatif = "0.17"
# Terminal colors
colored = "2"
# Hex dump formatting
hexyl-lib = "0.1"     # OR just manual hex formatting (no dep)

# Web framework (lightweight)
axum = "0.7"
tower = { version = "0.4", features = ["limit", "timeout"] }
tower-http = { version = "0.5", features = ["cors", "trace"] }

# Encryption (for backup)
aes-gcm = "0.10"

# LZ4 (from backup plan)
lz4_flex = "0.11"

# Timestamp for PITR
chrono = { version = "0.4", features = ["serde"] }

# --- already present ---
# tokio (full), serde, serde_json, pyo3, zstd, crc32fast, 
# hmac, sha2, argon2, bincode, rayon, parking_lot
```

### Binary targets:

```toml
[[bin]]
name = "qm"
path = "src/bin/qm.rs"

[[bin]]
name = "qm-web"
path = "src/bin/qm_web.rs"
```

**Impact on wheel size**: +500KB estimate (axum, clap, indicatif).

---

## 4. Implementation Phases

### Phase 0: Scaffold (Day 1)
| Task | File | LOC |
|---|---|---|
| Add deps to Cargo.toml | `Cargo.toml` | 15 |
| Create `src/bin/qm.rs` with clap | `bin/qm.rs` | 80 |
| Create `src/cli/mod.rs` with subcommands | `cli/mod.rs` | 120 |
| Create `src/web/mod.rs` stub | `web/mod.rs` | 30 |
| Add `mod cli; mod web;` to lib.rs | `lib.rs` | 2 |
| Verify cargo build | — | — |

**Subtotal**: ~250 LOC

### Phase 1: `qm inspect` + `qm stat` (ưu tiên cao nhất)
| Task | File | LOC |
|---|---|---|
| Page inspector + hex dump | `cli/inspect.rs` | 120 |
| WAL record browser | `cli/inspect.rs` | 50 |
| Snapshot inspector | `cli/inspect.rs` | 30 |
| Table metadata viewer | `cli/inspect.rs` | 40 |
| Live stat dashboard (TUI) | `cli/stat.rs` | 130 |
| One-shot stat (JSON/Prometheus) | `cli/stat.rs` | 50 |

**Subtotal**: ~420 LOC

### Phase 2: `qm check` + `qm dump`
| Task | File | LOC |
|---|---|---|
| Snapshot/WAL/table/index checker | `cli/check.rs` | 200 |
| Auto-repair logic | `cli/check.rs` | 80 |
| Streaming dump (sql/csv/jsonl) | `cli/dump.rs` | 120 |
| Parquet dump (reuse copy_to_parquet) | `cli/dump.rs` | 30 |

**Subtotal**: ~430 LOC

### Phase 3: Backup Enhanced (qm backup/restore/verify/predict)
Từ [backup-migration-suite-plan.md](backup-migration-suite-plan.md), bổ sung:

| Task | File | LOC |
|---|---|---|
| CLI wrappers cho backup module | `cli/backup.rs` | 100 |
| Incremental backup chain | `backup/backup.rs` | 100 |
| PITR restore (WAL timestamp) | `backup/restore.rs` | 80 |
| WalRecord timestamp field | `storage/wal.rs` | 30 |
| AES-256-GCM encrypt/decrypt | `backup/encrypt.rs` | 120 |

**Subtotal**: ~430 LOC

### Phase 4: `qm schema` + Web Dashboard
| Task | File | LOC |
|---|---|---|
| Schema diff engine | `cli/schema.rs` | 150 |
| Migration SQL generator | `cli/schema.rs` | 50 |
| axum Router + state | `web/mod.rs` | 80 |
| REST API endpoints (11 routes) | `web/api.rs` | 250 |
| Prometheus endpoint | `web/metrics.rs` | 20 |
| Dashboard HTML/CSS/JS | `static/dashboard.html` | 300 |
| `src/bin/qm_web.rs` entry point | `bin/qm_web.rs` | 40 |

**Subtotal**: ~890 LOC

### Phase 5: Polish & Testing
| Task | File | LOC |
|---|---|---|
| CLI integration tests | `tests/test_cli.py` | 100 |
| Web API tests | `tests/test_web.py` | 80 |
| `--help` documentation cho tất cả commands | `cli/*.rs` | 60 |
| Error messages & colored output | `cli/*.rs` | 40 |

**Subtotal**: ~280 LOC

---

## 5. Tổng kết

| Metric | Value |
|---|---|
| New Rust files | 14 (cli/ 6, web/ 4, bin/ 2, backup/ 2) |
| New HTML file | 1 (static/dashboard.html) |
| Total new LOC (Rust) | ~2,500 |
| Total new LOC (HTML/JS) | ~300 |
| Total new LOC (Tests) | ~180 |
| New crate deps | 7 (clap, indicatif, colored, axum, tower-http, aes-gcm, chrono) |
| Modified files | 3 (Cargo.toml, lib.rs, wal.rs) |
| Breaking changes | 0 |

### Priority order (nếu chỉ có 1 tuần):

```
1. qm inspect   ← Kiểm chứng LZ4/page ngay lập tức
2. qm stat      ← Monitoring cơ bản
3. qm check     ← Phát hiện corruption
4. qm backup    ← Production-ready backup
5. qm dump      ← Streaming export
6. Web Dashboard ← Visual monitoring
7. qm schema    ← Schema migration
```

> **Recommendation**: Bắt đầu với `qm inspect --page` trước. Nó chỉ ~120 LOC,
> dùng 100% infrastructure có sẵn (`PageHeader::from_bytes`, `WalRecord::decode`),
> và giúp verify ngay lập tức rằng storage layer hoạt động đúng trên disk.

---

## 6. Pre-Implementation Checklist

- [ ] Add `clap`, `indicatif`, `colored`, `axum`, `tower-http`, `aes-gcm`, `lz4_flex`, `chrono` to Cargo.toml
- [ ] Add `[[bin]]` sections for `qm` and `qm-web`
- [ ] Create `src/bin/`, `src/cli/`, `src/web/`, `static/` directories
- [ ] Verify `cargo build --bin qm` compiles
- [ ] Verify existing `cargo test` still passes (118 tests)
- [ ] Verify maturin wheel build still works (cdylib not broken by new bins)

*Ready for implementation. Start with Phase 0 scaffold + Phase 1 `qm inspect`.*
