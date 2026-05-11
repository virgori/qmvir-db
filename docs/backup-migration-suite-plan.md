# QMvir Backup & Migration Suite — Implementation Plan

**Version**: 2.1.0 &nbsp;|&nbsp; **Engine**: QMvir v2.0.0 base  
**Date**: 2026-04-02 (updated)  
**Module path**: `qm_engine/src/backup/`  
**Status**: Planning — no implementation started yet

---

## 1. Executive Summary

Build 5 production-grade backup/migration tools on top of existing QMvir
storage infrastructure. Reuses existing:

| Existing Infrastructure | Location | Reuse In |
|---|---|---|
| Zstd compression (level 3) | `storage/snapshot.rs` | qm_backup, qm_snapshot |
| CRC32 checksum | `snapshot.rs`, `wal.rs`, `uring_wal.rs` | All 5 tools |
| HMAC-SHA256 trailer | `snapshot.rs` (env: `QM_SNAPSHOT_HMAC_KEY`) | qm_backup, qm_verify |
| Atomic write (tmp→rename) | `native_sql/mod.rs` checkpoint + `snapshot.rs` | qm_backup, qm_restore |
| DirtyTracker + LSN | `snapshot.rs` (drain_dirty, PAGE_LSN_COUNTER) | qm_snapshot |
| bincode serialization | `native_sql/mod.rs` checkpoint/load_snapshot | qm_backup |
| Parquet I/O (COPY TO/FROM) | `native_sql/mod.rs` L1737–L2002 | qm_restore |
| WAL record CRC verify | `wal.rs` decode(), `native_sql/mod.rs` replay_wal | qm_verify |

> **Note**: `NativeSqlEngine.checkpoint()` uses plain bincode (no Zstd/CRC/HMAC).
> The `storage/snapshot.rs` Zstd+CRC+HMAC pipeline is for the page-level
> `SnapshotWriter` only. Backup tools should use the `snapshot.rs` pipeline,
> NOT the checkpoint pipeline.

**New Cargo dependency**: `lz4_flex = "0.11"` (pure Rust, no C dep).

---

## 2. Architecture Overview

```
qm_engine/src/
├── backup/
│   ├── mod.rs              — Public API, BackupConfig, shared types
│   ├── format.rs           — .qmbk file format codec (header/manifest/pages/footer)
│   ├── backup.rs           — qm_backup: logical + physical backup
│   ├── snapshot_diff.rs    — qm_snapshot: LSN-based differential/delta
│   ├── restore.rs          — qm_restore: .qmbk + PG dump restore
│   ├── predict.rs          — qm_predict: dry-run estimation
│   ├── verify.rs           — qm_verify: integrity + safety checks
│   └── pg_compat.rs        — Postgres dump lexer/translator
├── storage/                — (existing, untouched)
├── gateway/                — (existing, minor hookups)
└── lib.rs                  — Add PyO3 wrappers
```

All 5 tools operate directly on `NativeSqlEngine` internals — no TCP/IPC
needed. Each tool is a Rust struct with methods, exposed to Python via PyO3.

---

## 3. `.qmbk` File Format Specification

```
┌─────────────────────────────────────────────────────────┐
│ Header (64 bytes, fixed)                                │
│   magic:            u64  = 0x514D_424B_5550_0001        │
│   format_version:   u32  = 1                            │
│   backup_type:      u8   = Full(0) | Diff(1) | Table(2) │
│   compression:      u8   = None(0) | Lz4(1) | Zstd(2)  │
│   _reserved:        [u8; 2]                             │
│   base_lsn:         u64                                 │
│   end_lsn:          u64                                 │
│   timestamp:        u64  (unix secs)                    │
│   table_count:      u32                                 │
│   total_rows:       u64                                 │
│   original_size:    u64  (uncompressed bytes)           │
│   _pad:             [u8; 2]                             │
├─────────────────────────────────────────────────────────┤
│ Manifest (variable)                                     │
│   manifest_len:     u32                                 │
│   manifest_json:    [u8; manifest_len]                  │
│   — JSON: { tables: [{ name, columns, types,            │
│              row_count, data_offset, data_len }] }      │
├─────────────────────────────────────────────────────────┤
│ Table Data Blocks (per table)                           │
│   For each table:                                       │
│     table_name_len: u16                                 │
│     table_name:     [u8; table_name_len]                │
│     chunk_count:    u32                                 │
│     For each chunk (CHUNK_SIZE=1024 rows):              │
│       original_len:   u32                               │
│       compressed_len: u32                               │
│       data:           [u8; compressed_len]              │
│       — data = compress(bincode::serialize(rows))       │
├─────────────────────────────────────────────────────────┤
│ WAL Segment (optional, physical backup only)            │
│   wal_present:      u8 (0 or 1)                        │
│   wal_len:          u64                                 │
│   wal_data:         [u8; wal_len]                       │
├─────────────────────────────────────────────────────────┤
│ Footer (40 bytes, fixed)                                │
│   crc32:            u32  (covers Header..WAL)           │
│   hmac_sha256:      [u8; 32]  (0x00 if no key)         │
│   footer_magic:     u32  = 0x514D_454E_4421             │
└─────────────────────────────────────────────────────────┘
```

**Key design decisions:**
- Chunk-based table data enables parallel compression/decompression via rayon
- Manifest is JSON for human-inspectable `qm_verify --info`
- WAL segment included in physical backup for point-in-time recovery
- Footer CRC covers everything before it — single-pass verification

---

## 4. Tool Specifications

### 4.1 `qm_backup` — Logical & Physical Backup

**Purpose**: Full or table-level backup to `.qmbk` format.

**Modes**:
| Flag | Description |
|---|---|
| `--all` | Full backup (all tables + WAL) |
| `--tables t1,t2` | Selective table backup |
| `--compress lz4\|zstd\|none` | Compression algorithm (default: zstd) |
| `--include-wal` | Bundle WAL for physical backup |
| `--output path.qmbk` | Output file path |

**Implementation** (`backup/backup.rs`):

```rust
pub struct BackupEngine<'a> {
    engine: &'a NativeSqlEngine,
    config: BackupConfig,
}

pub struct BackupConfig {
    pub tables: Option<Vec<String>>,  // None = all
    pub compression: Compression,     // Lz4 | Zstd | None
    pub include_wal: bool,
    pub output: PathBuf,
}

impl<'a> BackupEngine<'a> {
    /// Create a backup. Acquires read lock on tables.
    pub fn run(&self) -> io::Result<BackupResult>;
}

pub struct BackupResult {
    pub path: PathBuf,
    pub tables_backed_up: usize,
    pub total_rows: u64,
    pub original_size: u64,
    pub compressed_size: u64,
    pub duration_ms: u64,
    pub crc32: u32,
}
```

**Data flow**:
1. Acquire `tables.read()` (no write lock — readers don't block)
2. Build manifest JSON from table metadata
3. For each table: serialize rows in chunks (1024), compress, write
4. Optionally append WAL file contents
5. Compute CRC32 + HMAC-SHA256 footer
6. Atomic write: `.qmbk.tmp` → rename → `.qmbk`

**Reuse**:
- `bincode::serialize` — same as `checkpoint()`
- `zstd::bulk::compress` — same as `SnapshotWriter`
- Atomic rename — same pattern as `checkpoint()`
- HMAC — same as `snapshot.rs`

**New code**: ~250 LOC

---

### 4.2 `qm_snapshot` — Differential/Delta Backup

**Purpose**: LSN-based incremental backup, storing only rows changed since a
reference LSN.

**Modes**:
| Flag | Description |
|---|---|
| `--since-lsn N` | Delta from specific LSN |
| `--since-last` | Delta from last backup's end_lsn |
| `--compress lz4\|zstd` | Compression (default: zstd) |
| `--output path.qmdiff` | Output file path |

**Implementation** (`backup/snapshot_diff.rs`):

```rust
pub struct DiffEngine<'a> {
    engine: &'a NativeSqlEngine,
    tracker: &'a DirtyTracker,
}

impl<'a> DiffEngine<'a> {
    /// Create a differential backup since `base_lsn`.
    pub fn create_diff(
        &self,
        base_lsn: u64,
        compression: Compression,
        output: &Path,
    ) -> io::Result<DiffResult>;
    
    /// Merge multiple diffs into a single consolidated diff.
    pub fn compact_diffs(
        diffs: &[PathBuf],
        output: &Path,
    ) -> io::Result<DiffResult>;
}
```

**`.qmdiff` format**: Same as `.qmbk` but with `backup_type = Diff(1)`. Only
contains table chunks for rows with LSN > `base_lsn`.

**LSN tracking strategy**:
- Current `DirtyTracker` tracks page-level changes (good for snapshot.rs page
  system)
- For logical backup, we need **row-level** LSN tracking
- **Approach**: Add `last_modified_lsn: u64` field to `NativeRow`
  - On INSERT/UPDATE/DELETE: stamp row with `PAGE_LSN_COUNTER.fetch_add(1)`
  - Diff engine: iterate all tables, collect rows where `last_modified_lsn > base_lsn`
  - DELETE tracking: maintain a `tombstone_log: Vec<(String, i64, u64)>` —
    `(table, row_id, lsn)` in `NativeSqlEngine`

**Schema change** (non-breaking — bincode backward compat via `#[serde(default)]`):
```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRow {
    pub cols: HashMap<String, Cell>,
    #[serde(default)]
    pub last_modified_lsn: u64,   // NEW
}
```

**New code**: ~200 LOC (diff engine) + ~30 LOC (NativeRow LSN stamping)

---

### 4.3 `qm_restore` — Restore & PG Compatibility

**Purpose**: Restore from `.qmbk`/`.qmdiff` files, or import from Postgres
`pg_dump` SQL output.

**Modes**:
| Flag | Description |
|---|---|
| `--from path.qmbk` | Restore from QMvir backup |
| `--from path.sql` | Import from pg_dump SQL |
| `--tables t1,t2` | Selective restore (only specific tables) |
| `--drop-existing` | DROP TABLE IF EXISTS before restore |
| `--dry-run` | Parse & validate without writing |
| `--verify` | Verify CRC/HMAC before restore |

**Implementation** (`backup/restore.rs` + `backup/pg_compat.rs`):

```rust
pub struct RestoreEngine<'a> {
    engine: &'a mut NativeSqlEngine,
}

impl<'a> RestoreEngine<'a> {
    /// Restore from .qmbk file.
    pub fn restore_qmbk(&self, path: &Path, opts: RestoreOpts) -> io::Result<RestoreResult>;
    
    /// Restore from .qmdiff (apply delta on top of current state).
    pub fn restore_diff(&self, path: &Path) -> io::Result<RestoreResult>;
    
    /// Import from pg_dump SQL file.
    pub fn import_pgdump(&self, path: &Path, opts: RestoreOpts) -> io::Result<RestoreResult>;
}
```

**Postgres dump parser** (`pg_compat.rs`):

Handles a subset of pg_dump output — enough for data migration, not full PG
SQL compatibility:

| PG Syntax | QMvir Translation |
|---|---|
| `CREATE TABLE ... (col TYPE, ...)` | Map PG types to ColType |
| `COPY table FROM stdin;` ... `\.` | Bulk row insert |
| `INSERT INTO ... VALUES (...)` | Standard execute |
| `SET client_encoding = 'UTF8'` | Skip (always UTF-8) |
| `SERIAL` / `BIGSERIAL` | Map to `INTEGER` with auto-increment |
| `VARCHAR(n)` / `TEXT` / `CHAR(n)` | Map to `Text` |
| `INTEGER` / `BIGINT` / `SMALLINT` | Map to `Integer` |
| `REAL` / `DOUBLE PRECISION` / `NUMERIC` | Map to `Float8` |
| `BOOLEAN` | Map to `Integer` (0/1) |
| `TIMESTAMP` / `DATE` | Map to `Text` (ISO-8601 string) |
| `SELECT setval(...)` | Skip |
| `ALTER TABLE ... ADD CONSTRAINT` | Skip (no FK support yet) |

**PG COPY parser** — the key component:
```rust
/// Parse PG COPY format: tab-separated values, \N for NULL, \. terminator.
fn parse_pg_copy_block(
    lines: &[&str],
    columns: &[String],
    col_types: &[ColType],
) -> Vec<NativeRow>;
```

**Restore data flow (`.qmbk`)**:
1. Read & verify footer CRC + HMAC
2. Parse header → extract manifest
3. For each table in manifest:
   a. Optionally drop existing table
   b. CREATE TABLE with stored schema
   c. Decompress chunks → bincode::deserialize → insert rows
4. Optionally replay WAL segment for point-in-time
5. Trigger `checkpoint()` after restore

**New code**: ~300 LOC (restore) + ~250 LOC (pg_compat)

---

### 4.4 `qm_predict` — Dry Run & Estimation

**Purpose**: Estimate backup size, time, and resources without writing data.

**Modes**:
| Flag | Description |
|---|---|
| `--backup` | Estimate full backup size |
| `--diff --since-lsn N` | Estimate diff size |
| `--restore path` | Estimate restore time & space needed |
| `--json` | Output as JSON |

**Implementation** (`backup/predict.rs`):

```rust
pub struct PredictEngine<'a> {
    engine: &'a NativeSqlEngine,
}

#[derive(Serialize)]
pub struct Prediction {
    pub estimated_original_bytes: u64,
    pub estimated_compressed_bytes: u64,
    pub compression_ratio: f64,
    pub table_estimates: Vec<TableEstimate>,
    pub estimated_duration_ms: u64,
    pub disk_space_available: u64,
    pub sufficient_space: bool,
}

#[derive(Serialize)]
pub struct TableEstimate {
    pub name: String,
    pub row_count: u64,
    pub estimated_bytes: u64,
    pub column_count: usize,
}

impl<'a> PredictEngine<'a> {
    pub fn predict_backup(&self, config: &BackupConfig) -> Prediction;
    pub fn predict_diff(&self, base_lsn: u64) -> Prediction;
    pub fn predict_restore(&self, path: &Path) -> io::Result<Prediction>;
}
```

**Estimation algorithm**:
1. **Row size**: For each table, sample up to 100 rows → `bincode::serialized_size()`
2. **Compression ratio**: Use known ratios — Zstd ≈ 0.35–0.45, LZ4 ≈ 0.55–0.65
   (calibrated from actual data characteristics)
3. **Throughput**: Use `200 MB/s` for Zstd, `600 MB/s` for LZ4 as baseline
4. **Disk space**: `std::fs::metadata` on data_dir filesystem + `statvfs` on unix
5. **Diff estimation**: Count rows where `last_modified_lsn > base_lsn`

**New code**: ~120 LOC

---

### 4.5 `qm_verify` — Data Integrity & Safety Checks

**Purpose**: Verify backup file integrity and validate restore safety.

**Modes**:
| Flag | Description |
|---|---|
| `--file path.qmbk` | Verify a backup file |
| `--info` | Show backup metadata (manifest, size, tables) |
| `--deep` | Full decompression + row count verification |
| `--wal path` | Verify WAL file CRC integrity |
| `--checksum` | Recompute & compare CRC32 + HMAC |

**Implementation** (`backup/verify.rs`):

```rust
pub struct VerifyEngine;

#[derive(Serialize)]
pub struct VerifyResult {
    pub path: String,
    pub valid: bool,
    pub header: Option<BackupHeader>,
    pub manifest: Option<serde_json::Value>,
    pub crc32_ok: bool,
    pub hmac_ok: Option<bool>,  // None if no HMAC key
    pub table_checks: Vec<TableCheck>,
    pub errors: Vec<String>,
}

#[derive(Serialize)]
pub struct TableCheck {
    pub name: String,
    pub expected_rows: u64,
    pub actual_rows: Option<u64>,  // Only with --deep
    pub chunks_ok: bool,
}

impl VerifyEngine {
    /// Quick verify: CRC32 + HMAC only (single-pass read).
    pub fn verify_quick(path: &Path) -> io::Result<VerifyResult>;
    
    /// Deep verify: decompress all chunks, count rows.
    pub fn verify_deep(path: &Path) -> io::Result<VerifyResult>;
    
    /// Show backup info without verification.
    pub fn info(path: &Path) -> io::Result<VerifyResult>;
    
    /// Verify WAL file integrity.
    pub fn verify_wal(path: &Path) -> io::Result<WalVerifyResult>;
}
```

**Verification flow**:
1. Read header → validate magic + version
2. Stream-compute CRC32 over all bytes until footer
3. Compare with stored CRC32
4. If HMAC key set → verify HMAC-SHA256
5. (Deep mode) Decompress each chunk → deserialize → count rows vs manifest

**New code**: ~180 LOC

---

## 5. Dependency Changes

### Cargo.toml additions:

```toml
# LZ4 compression for backup (pure Rust, no C dependency)
lz4_flex = "0.11"
```

No other new crates needed — all other functionality (zstd, crc32fast, hmac,
sha2, bincode, serde_json, rayon) already in dependencies.

### statvfs for disk space prediction (unix only):

Already have `libc = "0.2"` on `cfg(target_os = "linux")`. Extend to:
```toml
[target.'cfg(unix)'.dependencies]
libc = "0.2"
```

---

## 6. Schema Changes

### 6.1 `NativeRow` — Add LSN field

```rust
// gateway/native_sql/mod.rs
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct NativeRow {
    pub cols: HashMap<String, Cell>,
    #[serde(default)]
    pub last_modified_lsn: u64,
}
```

**Backward compatibility**: `#[serde(default)]` means old snapshots (without
this field) deserialize with `last_modified_lsn = 0`. No migration needed.

### 6.2 `NativeSqlEngine` — Add tombstone log

```rust
// gateway/native_sql/mod.rs (in NativeSqlEngine struct)
tombstone_log: Arc<PLRwLock<Vec<(String, i64, u64)>>>,  // (table, row_id, lsn)
```

Tombstones are written on DELETE and consumed/cleared by `qm_snapshot`.
Persisted to `data_dir/tombstones.bincode` during checkpoint.

---

## 7. PyO3 Bindings

Add to `lib.rs`:

```rust
// backup/mod.rs re-exports
#[pyclass]
pub struct PyBackupEngine { /* wraps BackupEngine */ }

#[pymethods]
impl PyBackupEngine {
    #[new]
    fn new(engine: &PyNativeSqlEngine) -> Self;
    
    fn backup(&self, tables: Option<Vec<String>>, compress: &str, 
              include_wal: bool, output: &str) -> PyResult<PyObject>;
    
    fn predict(&self, tables: Option<Vec<String>>, compress: &str) -> PyResult<PyObject>;
}

#[pyclass]
pub struct PyRestoreEngine { /* wraps RestoreEngine */ }

#[pymethods]
impl PyRestoreEngine {
    fn restore(&self, path: &str, drop_existing: bool, verify: bool) -> PyResult<PyObject>;
    fn import_pgdump(&self, path: &str, drop_existing: bool) -> PyResult<PyObject>;
}

#[pyclass]
pub struct PyVerifyEngine;

#[pymethods]
impl PyVerifyEngine {
    #[staticmethod]
    fn verify(path: &str, deep: bool) -> PyResult<PyObject>;
    
    #[staticmethod]
    fn info(path: &str) -> PyResult<PyObject>;
}
```

---

## 8. Implementation Phases

### Phase 1: Foundation (backup/mod.rs + backup/format.rs)
**Priority**: Must be first — all tools depend on this.

| Task | File | LOC | Depends On |
|---|---|---|---|
| Define `Compression` enum (None/Lz4/Zstd) | `mod.rs` | 30 | — |
| Define `BackupHeader` struct + to/from_bytes | `format.rs` | 80 | — |
| Define `BackupFooter` struct + verify methods | `format.rs` | 50 | — |
| Manifest JSON schema | `format.rs` | 40 | — |
| Chunk compress/decompress helpers | `mod.rs` | 60 | lz4_flex crate |
| Add `lz4_flex` to Cargo.toml | `Cargo.toml` | 1 | — |
| Add `NativeRow.last_modified_lsn` field | `native_sql/mod.rs` | 5 | — |
| LSN stamping on INSERT/UPDATE/DELETE | `native_sql/mod.rs` | 20 | NativeRow change |
| Tombstone log on DELETE | `native_sql/mod.rs` | 30 | — |
| Tombstone persist in checkpoint | `native_sql/mod.rs` | 15 | — |

**Subtotal**: ~330 LOC

### Phase 2: qm_backup + qm_verify
**Priority**: Core backup/verify — most immediate user value.

| Task | File | LOC |
|---|---|---|
| `BackupEngine::run()` | `backup.rs` | 200 |
| `BackupConfig` / `BackupResult` | `backup.rs` | 50 |
| `VerifyEngine::verify_quick()` | `verify.rs` | 80 |
| `VerifyEngine::verify_deep()` | `verify.rs` | 60 |
| `VerifyEngine::info()` | `verify.rs` | 40 |
| `VerifyEngine::verify_wal()` | `verify.rs` | 30 |
| PyO3 wrappers for backup+verify | `lib.rs` | 60 |
| Unit tests | `backup/tests.rs` | 80 |
| Python integration tests | `tests/test_backup.py` | 60 |

**Subtotal**: ~660 LOC

### Phase 3: qm_snapshot (differential)
**Priority**: Depends on Phase 1 LSN stamping being exercised.

| Task | File | LOC |
|---|---|---|
| `DiffEngine::create_diff()` | `snapshot_diff.rs` | 130 |
| `DiffEngine::compact_diffs()` | `snapshot_diff.rs` | 70 |
| Tombstone consumption logic | `snapshot_diff.rs` | 30 |
| PyO3 wrapper | `lib.rs` | 30 |
| Tests | `tests/test_snapshot_diff.py` | 50 |

**Subtotal**: ~310 LOC

### Phase 4: qm_restore + PG compat
**Priority**: Can be developed in parallel with Phase 3.

| Task | File | LOC |
|---|---|---|
| `RestoreEngine::restore_qmbk()` | `restore.rs` | 150 |
| `RestoreEngine::restore_diff()` | `restore.rs` | 80 |
| `RestoreEngine::import_pgdump()` | `restore.rs` | 70 |
| PG type mapping table | `pg_compat.rs` | 50 |
| PG COPY block parser | `pg_compat.rs` | 100 |
| PG CREATE TABLE translator | `pg_compat.rs` | 80 |
| PG statement router (skip/translate) | `pg_compat.rs` | 50 |
| PyO3 wrapper | `lib.rs` | 40 |
| Tests (including real pg_dump samples) | `tests/test_restore.py` | 80 |

**Subtotal**: ~700 LOC

### Phase 5: qm_predict
**Priority**: Lowest — nice-to-have, no blocking dependencies.

| Task | File | LOC |
|---|---|---|
| `PredictEngine::predict_backup()` | `predict.rs` | 60 |
| `PredictEngine::predict_diff()` | `predict.rs` | 30 |
| `PredictEngine::predict_restore()` | `predict.rs` | 30 |
| Disk space check (statvfs) | `predict.rs` | 25 |
| PyO3 wrapper | `lib.rs` | 20 |
| Tests | `tests/test_predict.py` | 30 |

**Subtotal**: ~195 LOC

---

## 9. Total Estimates

| Metric | Value |
|---|---|
| New Rust files | 8 (`backup/` module) |
| New Python test files | 4 |
| Total new LOC (Rust) | ~2,200 |
| Total new LOC (Python tests) | ~220 |
| Modified files | 3 (Cargo.toml, native_sql/mod.rs, lib.rs) |
| New crate dependencies | 1 (lz4_flex) |
| Schema changes | 2 (NativeRow.last_modified_lsn, tombstone_log) |
| Breaking changes | 0 (all backward compatible) |

---

## 10. Risk Analysis

| Risk | Impact | Mitigation |
|---|---|---|
| NativeRow LSN field breaks old snapshots | Medium | `#[serde(default)]` — auto 0 for missing |
| LZ4 adds wheel size | Low | lz4_flex is pure Rust, ~50KB |
| PG dump parser doesn't cover all syntax | Medium | Explicit skip-list for unsupported; warn user |
| Large backup holds read lock too long | High | Use `tables.read()` (non-exclusive) + chunked iteration |
| HMAC key management for backups | Medium | Same env var as snapshot.rs (`QM_SNAPSHOT_HMAC_KEY`) |
| Cross-platform disk space check | Low | `#[cfg(unix)]` statvfs, fallback to None on Windows |

---

## 11. Testing Strategy

### Unit tests (Rust, in `backup/` module):
- Header encode/decode roundtrip
- Chunk compress/decompress with all 3 algorithms
- CRC32 + HMAC verification
- Manifest serialization

### Integration tests (Python):
- `test_backup.py`: Full → verify → restore roundtrip
- `test_snapshot_diff.py`: INSERT → diff → more INSERT → diff → restore both
- `test_restore.py`: Import pg_dump with COPY blocks, type mapping
- `test_predict.py`: Estimate vs actual backup size (within 2x tolerance)

### Regression:
- All existing **212 tests** (202 pass + 10 xfail) must pass after NativeRow schema change
- Checkpoint/load_snapshot roundtrip with new field
- Parquet COPY TO/FROM roundtrip with new field

---

## 12. Implementation Order & Dependencies

```
Phase 1 (Foundation)
  ├── Cargo.toml + lz4_flex
  ├── NativeRow.last_modified_lsn
  ├── LSN stamping in execute_inner()
  ├── backup/mod.rs (types, compress helpers)
  └── backup/format.rs (header, footer, manifest)
       │
       ├── Phase 2 (qm_backup + qm_verify)  ←── Start here for immediate value
       │     ├── backup/backup.rs
       │     ├── backup/verify.rs
       │     └── PyO3 + tests
       │
       ├── Phase 3 (qm_snapshot)  ←── Parallel with Phase 4
       │     └── backup/snapshot_diff.rs
       │
       ├── Phase 4 (qm_restore + PG compat)  ←── Parallel with Phase 3
       │     ├── backup/restore.rs
       │     └── backup/pg_compat.rs
       │
       └── Phase 5 (qm_predict)
             └── backup/predict.rs
```

---

## 13. Pre-Implementation Checklist

- [ ] Add `lz4_flex = "0.11"` to `[dependencies]` in Cargo.toml
- [ ] Change `[target.'cfg(target_os = "linux")'.dependencies]` to `[target.'cfg(unix)'.dependencies]`
- [ ] Add `last_modified_lsn: u64` to `NativeRow` with `#[serde(default)]`
- [ ] Add tombstone_log field to `NativeSqlEngine`
- [ ] Add `mod backup;` to `qm_engine/src/lib.rs`
- [ ] Create `qm_engine/src/backup/` directory
- [ ] Verify all 212 existing tests still pass after schema changes (202 pass + 10 xfail)
- [ ] Build & verify cross-platform wheels still compile

---

*Ready for implementation. Start with Phase 1 + Phase 2 for first usable backup/verify.*

---

## 14. Current Codebase State (as of 2026-04-02)

Everything below is a pre-implementation audit — confirms exactly what exists
and what needs to be created.

### What already exists (reusable)

| Component | File | Notes |
|---|---|---|
| `NativeRow` | `native_sql/mod.rs` L99 | Only has `cols: HashMap<String, Cell>` — **needs** `last_modified_lsn` |
| `NativeSqlEngine` | `native_sql/mod.rs` L654 | 10 fields — **no** `tombstone_log` yet |
| `DirtyTracker` + LSN | `storage/snapshot.rs` L51 | Page-level dirty tracking, `drain_dirty()`, `advance_lsn()` |
| `SnapshotWriter/Reader` | `storage/snapshot.rs` | Zstd + CRC32 + optional HMAC pipeline (page-level) |
| `checkpoint()` | `native_sql/mod.rs` L815 | Plain `bincode::serialize` → atomic rename, no compression/CRC |
| `load_snapshot()` | `native_sql/mod.rs` L792 | `bincode::deserialize` with JSON fallback |
| Parquet COPY TO | `native_sql/mod.rs` L1816 | `COPY table TO 'path' (FORMAT PARQUET)` |
| Parquet COPY FROM | `native_sql/mod.rs` L1898 | Bulk-load from `.parquet`, path-traversal protected |
| WAL replay | `native_sql/mod.rs` | CRC-verified replay from `native_sql.wal` |
| Dependencies | `Cargo.toml` | `zstd`, `crc32fast`, `hmac`, `sha2`, `bincode`, `rayon`, `arrow`, `parquet` all present |

### What does NOT exist yet (to implement)

| Component | Plan Section |
|---|---|
| `qm_engine/src/backup/` directory | §2 |
| `NativeRow.last_modified_lsn` field | §6.1 |
| `NativeSqlEngine.tombstone_log` field | §6.2 |
| `lz4_flex` dependency in Cargo.toml | §5 |
| `mod backup;` in `lib.rs` | §7 |
| PyO3 backup/restore/verify classes | §7 |
| All 8 Rust files in backup module | §2, §4.1–4.5 |
| Python test files for backup suite | §8, §11 |

### PyO3 classes currently registered in lib.rs

`PyPostgresGateway`, `PyNativeSqlEngine`, `PySqlParser`, `PyVectorExecutor`,
`PyStorageEngine`, `PyTransaction`, `PyIndexManager`, `PyHubEngine`,
`PyRingBuffer`, `PyNativeDispatcher`, `PyWTinyLfuCache`, `PyUringWalWriter`,
`PyJitCompiler`, `PyShardRing`, `PyShardManager` — **15 classes total**.

New backup module will add: `PyBackupEngine`, `PyRestoreEngine`, `PyVerifyEngine` (3 more).

---

## 15. Engine Quirks & Constraints (discovered via test_full_engine.py)

These directly affect backup/restore implementation:

| Quirk | Impact on Backup Suite |
|---|---|
| INSERT requires explicit column names: `INSERT INTO t (c1,c2) VALUES (...)` | `pg_compat.rs` PG COPY translator MUST emit explicit column names |
| `SELECT *` returns all columns (no projection) | Restore verification should use `SELECT *` only |
| `WHERE col > N` (float comparison) not supported in pgwire | Restore validation cannot use range queries |
| `UPDATE SET` on INT columns may crash | Restore should use DELETE+INSERT for row replacement |
| `JOIN` returns 0 rows via pgwire | Cross-table verification not possible via SQL |
| `checkpoint()` uses plain bincode (no Zstd/CRC) | Backup format (`.qmbk`) is independent — uses its own Zstd+CRC pipeline |
| Auto-checkpoint every 10,000 WAL mutations | WAL bundling in physical backup must account for mid-backup checkpoint |

### Design implications

1. **`qm_restore` — pg_dump import**: The COPY block translator in `pg_compat.rs`
   must rewrite `INSERT INTO t VALUES (...)` → `INSERT INTO t (col1, col2, ...)
   VALUES (...)` using the table's column list from `CREATE TABLE`.

2. **`qm_backup` — data serialization**: Should serialize via
   `bincode::serialize(&table.rows)` directly (same as checkpoint), NOT via SQL
   queries. This avoids all pgwire quirks.

3. **`qm_verify` — row count validation**: Use `table.rows.len()` in Rust, NOT
   `SELECT COUNT(*)` (which works but is slower and goes through pgwire).

4. **`qm_restore` — UPDATE avoidance**: For diff restore, prefer
   `table.rows.insert(key, row)` direct HashMap insertion instead of SQL UPDATE.

---

## 16. Cross-references

| Document | Reference to This Plan |
|---|---|
| [cli-web-dashboard-plan.md](cli-web-dashboard-plan.md) §1.5 | CLI commands `qm backup/restore/verify/predict` call into `src/backup/` |
| [cli-web-dashboard-plan.md](cli-web-dashboard-plan.md) §0 | Architecture shows `src/backup/` as dependency of `src/cli/backup.rs` |
| [qmvir-studio-native-app-plan.md](qmvir-studio-native-app-plan.md) Feature X4 | "Backup Manager" GUI wraps the 4 backup tools via Tauri commands |
| [qmvir-studio-native-app-plan.md](qmvir-studio-native-app-plan.md) Phase 5 | ~480 LOC Tauri+React for `BackupManager.tsx` + `commands/backup.rs` |

---

## 17. Test Suite Baseline (pre-implementation)

```
$ python -m pytest tests/ -q
202 passed, 10 xfailed in 18.97s

Test files (12 active):
  test_full_engine.py    — 94 tests (84 pass, 10 xfail) — comprehensive SQL engine tests
  test_rust_engine.py    — 60 tests — core Rust engine via PyO3
  test_schema_action.py  —  9 tests — schema DDL operations
  test_cache.py          —  8 tests — WTinyLFU cache
  test_vector.py         —  8 tests — vector operations
  test_btree.py          —  7 tests — B-tree index
  test_planner.py        —  7 tests — query planner
  test_columnar.py       —  6 tests — columnar storage
  test_mvcc.py           —  6 tests — MVCC transactions
  test_bm25.py           —  5 tests — BM25 text search
  test_restart.py        —  2 tests — restart persistence
  Total:                  212 tests
```

All Phase 1 schema changes must preserve this baseline.
