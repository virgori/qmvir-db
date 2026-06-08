# QMvir Database — Full Codebase Audit Report

> **Date**: 2026-04-10  
> **Auditor**: Antigravity AI (Claude Opus 4.6 Thinking)  
> **Codebase**: `/Users/gengyang/Desktop/AI/QM`  
> **Scale**: ~54,800 LOC Python · ~44,400 LOC Rust · ~1,200 LOC C · 37 test files

---

## Table of Contents

1. [Executive Summary](#1-executive-summary)
2. [Security Vulnerabilities](#2-security-vulnerabilities)
3. [Data Protection Issues](#3-data-protection-issues)
4. [Feature Bugs](#4-feature-bugs)
5. [SQL Engine — Missing Features](#5-sql-engine--missing-features)
6. [CLI Bugs & Issues](#6-cli-bugs--issues)
7. [Architecture Evaluation](#7-architecture-evaluation)
8. [Recommendations Priority Matrix](#8-recommendations-priority-matrix)

---

## 1. Executive Summary

QMvir is an ambitious hybrid AI-native database system with a Hub-Satellite IPC architecture, dual Python/Rust engine path, PostgreSQL wire protocol compatibility, vector search (HNSW), full-text search (BM25/WAND), PL/QM stored procedures, backup/restore with encryption, and an analytics platform.

### Overall Assessment

| Category | Rating | Risk |
|---|---|---|
| **Security** | ⚠️ Medium-High | 7 critical, 5 high, 8 medium findings |
| **Data Protection** | ⚠️ Medium | Missing RBAC enforcement in data path, no encryption at rest |
| **SQL Completeness** | 🔶 Moderate | Core DML/DDL present, many gaps for production SQL compliance |
| **CLI** | ✅ Good | Minor issues, well-structured |
| **Architecture** | ✅ Strong | Clean separation of concerns, good abstractions |

---

## 2. Security Vulnerabilities

### 🔴 CRITICAL — SEC-01: SQL Injection via F-String Interpolation

**Files**: `qm_core/bench.py`, `tools/qm_cli.py`, multiple benchmark files

The CLI tools and benchmarks construct SQL via Python f-strings with unsanitized user/runtime values:

```python
# tools/qm_cli.py:168 — table name directly interpolated
cols, rows, _ = engine.execute(f"SELECT * FROM {args.table} LIMIT 20")

# tools/qm_cli.py:238
_, cr, tag = engine.execute(f"SELECT COUNT(*) FROM {name}")

# tools/qm_cli.py:274
cols, rows, _ = engine.execute(f"SELECT * FROM {table_name}")

# tools/qm_cli.py:295 — SQL dump generates INSERT via string concat
out.write(f"INSERT INTO {table_name} ({col_list}) VALUES ({', '.join(vals)});\\n")
```

**Impact**: An attacker who controls table names or data values can inject arbitrary SQL through the CLI dump/inspect/check commands. The `qm sql` passthrough also sends raw SQL without parameterization.

**Fix**: 
- Implement identifier quoting (double-quote table/column names)
- Add parameterized query support to the SQL engine
- Validate identifiers against `^[a-zA-Z_][a-zA-Z0-9_]*$`

---

### 🔴 CRITICAL — SEC-02: Default Admin Password Hardcoded

**File**: `qm_core/auth.py:162`

```python
_DEFAULT_ADMIN_PASS = "admin"     # overridden by env QM_ADMIN_PASSWORD
```

**And in Dockerfile**:
```dockerfile
ENV QM_ADMIN_PASSWORD=changeme
```

**Impact**: If `QM_ADMIN_PASSWORD` is not set, the database starts with `admin/admin` credentials. The Dockerfile uses `changeme` which is only marginally better. No enforcement exists to require password change at first login.

**Fix**:
- Refuse to start with the default password when binding to non-loopback addresses
- Implement a first-run password setup wizard
- Add `--require-password-change` flag

---

### 🔴 CRITICAL — SEC-03: No TLS/SSL Support — Credentials Sent in Cleartext

**Files**: `gateway/api_postgres/server.py`, `qm_engine/src/gateway/connection.rs`

```python
# gateway/api_postgres/server.py:138
writer.write(b"N")  # Reject SSL

# Rust gateway (connection.rs:229)
# Message::SSLRequest => { // Deny SSL for now (send 'N') }
```

Both the Python and Rust PostgreSQL gateways **actively reject SSL/TLS**. All authentication happens over cleartext TCP — passwords are visible to any network sniffer.

**Impact**: Complete credential exposure on any network segment. Violates every compliance framework (PCI-DSS, HIPAA, SOC2, GDPR).

**Fix**:
- Add `rustls` or `native-tls` to the Rust gateway
- Add Python `ssl` context wrapping for the asyncio server
- Make TLS mandatory by default with opt-out `--no-tls` flag

---

### 🔴 CRITICAL — SEC-04: No Authentication Enforcement on PostgreSQL Wire Protocol

**File**: `gateway/api_postgres/server.py:100, 143-147`

```python
session = PgSession(execute_fn=self._executor, pid=pid, secret=os.getpid())
# ...
response = session.handle_startup(payload)
writer.write(response)
# Immediately proceeds to message loop — NO password check
```

The PostgreSQL gateway calls `handle_startup()` which sends `AuthenticationOk` immediately without ever verifying credentials. The `UserCatalog` exists but is never consulted during connection establishment.

**Impact**: Any client can connect without authentication and execute arbitrary SQL, including DDL and admin operations.

**Fix**:
- Integrate `UserCatalog.authenticate()` into `PgSession.handle_startup()`
- Send `AuthenticationCleartextPassword` or `AuthenticationMD5Password` message
- Read the password response before sending `AuthenticationOk`
- Pass `AuthSession` to executor for per-query RBAC checks

---

### 🔴 CRITICAL — SEC-05: RBAC `check_permission()` Never Called in Data Path

**Files**: `qm_core/auth.py:273-298`, `gateway/api_postgres/hub_executor.py`

The `check_permission(session, ast_node)` function exists but is **never invoked** by the hub executor or engine. There is no code path that gates SQL execution on the caller's role.

```python
# hub_executor.py — executor created WITHOUT any auth session
def make_hub_executor(engine, media_allocator=None, checkpoint_manager=None) -> ExecuteFn:
    # No AuthSession parameter, no permission checking
    def _execute(sql: str) -> tuple[...]:
        results = engine.execute_sql(sql)  # Direct execution, no ACL
```

**Impact**: Even if authentication were fixed, all users would have ADMIN-level access to all operations.

**Fix**:
- Thread `AuthSession` through `make_hub_executor`
- Call `check_permission(session, ast)` after SQL parsing, before execution
- Implement per-table ACLs (GRANT/REVOKE)

---

### 🔴 CRITICAL — SEC-06: `unsafe` Rust Code Without Alignment Guarantees

**File**: `qm_engine/src/types.rs:85-100`

```rust
pub fn as_f32_slice(&self) -> &[f32] {
    debug_assert_eq!(self.element_size, 4);
    // SAFETY: Bytes buffer is aligned and was created from f32 values
    unsafe {
        std::slice::from_raw_parts(self.data.as_ptr() as *const f32, self.len)
    }
}
```

**38 `unsafe` blocks** identified across the Rust codebase. The `ZeroCopyBuffer` casts `Bytes` to typed slices with a **`debug_assert`** for alignment that is **compiled out in release mode**. `bytes::Bytes` does NOT guarantee alignment — `Bytes::slice()` can return unaligned buffers.

**Similar patterns** in:
- `src/ipc/ring_buffer.rs` — raw pointer arithmetic on mmap'ed memory
- `src/executor/vectorized.rs` — SIMD intrinsics
- `src/storage/page.rs` — raw page buffer access

**Impact**: Undefined behavior in release builds. Potential for memory corruption, data loss, and security exploits.

**Fix**:
- Add runtime alignment checks or use `bytemuck::try_cast_slice`
- Replace `debug_assert!` with `assert!` for safety-critical invariants
- Audit all 38 `unsafe` blocks for soundness

---

### 🔴 CRITICAL — SEC-07: LIKE/Regex Pattern Injection

**Files**: `qm_core/engine.py:895-904`, `qm_core/hub_engine.py:812-821`

```python
def _like(row, e=expr_fn, p=pat_fn, n=neg):
    import re as _re
    v = e(row)
    pat = p(row)
    regex = pat.replace("%", ".*").replace("_", ".")
    matched = bool(_re.match(f"^{regex}$", str(v), _re.IGNORECASE))
```

The `LIKE` expression compiles user-supplied patterns directly to regex without escaping regex metacharacters. A pattern like `%(.*)%` would be interpreted as a regex capture group. Patterns with `+`, `?`, `{`, `[`, `\` etc. could cause `re.error` crashes or ReDoS.

**Fix**:
- `re.escape()` the pattern before replacing `%` and `_`
- Or implement LIKE matching without regex

---

### 🟠 HIGH — SEC-08: Division by Zero in Expression Compiler

**File**: `qm_core/engine.py:873`, `qm_core/hub_engine.py:790`

```python
if op == "/":
    return lambda row, l=left_fn, r=right_fn: (l(row) or 0) / (r(row) or 1)
```

When `r(row)` returns `0` (a falsy value), the `or 1` kicks in, silently changing the divisor. But if `r(row)` returns `0.0` (also falsy), **the same silent change occurs**. This is a semantic bug, not a crash, but it silently returns wrong results.

For actual division by zero when r(row) returns a non-falsy zero-like value, `ZeroDivisionError` would crash the query.

**Fix**: Explicit check: `divisor = r(row); return None if divisor == 0 else l(row) / divisor`

---

### 🟠 HIGH — SEC-09: No Rate Limiting on Authentication

**File**: `qm_core/auth.py:220-229`

The `authenticate()` method has no protection against brute-force attacks:
- No account lockout after N failed attempts
- No exponential backoff
- No rate limiting per IP
- No `fail2ban`-style integration

**Fix**: Add per-user failed attempt counter with exponential backoff and lockout after configurable threshold.

---

### 🟠 HIGH — SEC-10: Session Tokens Not Validated Post-Auth

**File**: `qm_core/auth.py:228`

```python
token = secrets.token_hex(16)
return AuthSession(username=rec.username, role=rec.role, token=token)
```

Tokens are generated but never stored in a session registry. There's no way to:
- Validate a token on subsequent requests
- Revoke a compromised session
- Enforce session timeouts/expiry

---

### 🟠 HIGH — SEC-11: PID File Race Condition (Symlink Attack)

**File**: `qm_app.py:441-442`

```python
pid_path = self._data_dir / _PID_FILE
pid_path.write_text(str(os.getpid()))
```

No `O_EXCL` or symlink check before writing. An attacker with local access could create a symlink at the PID file location pointing to a sensitive file — the daemon would overwrite it.

**Fix**: Use `os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL)` or check `os.path.islink()`.

---

### 🟠 HIGH — SEC-12: Unvalidated Media File Path (Path Traversal)

**File**: `gateway/api_postgres/hub_executor.py:134-136`

```python
if isinstance(ast, LinkMediaStmt):
    if not os.path.exists(ast.path):
        raise RuntimeError(f"Media file not found: {ast.path}")
    with open(ast.path, "rb") as f:
        data = f.read()
```

The `LINK MEDIA` SQL extension accepts arbitrary file paths from the SQL client and reads them into the media allocator. No path sanitization is performed. A user could read `/etc/passwd` or any file readable by the daemon process.

**Fix**: 
- Restrict `ast.path` to a configured media directory
- Canonicalize and validate `os.path.realpath(ast.path).startswith(allowed_dir)`

---

### 🟡 MEDIUM — SEC-13 to SEC-20

| ID | Issue | File | Impact |
|---|---|---|---|
| SEC-13 | `os.getpid()` used as BackendKeyData secret — predictable | `server.py:100` | Cancel-request spoofing |
| SEC-14 | `hashlib.md5` used for query hashing (weak hash) | `engine.py:587` | Low (non-security context) |
| SEC-15 | Background worker silently swallows all exceptions | `concurrency.py:229` | Missed security events |
| SEC-16 | No input validation on `vector_dim` (can be negative) | `qm_app.py:127` | DoS via invalid allocation |
| SEC-17 | Audit log is in-memory only — not WAL-backed | `audit.py:74` | Lost audit trail on crash |
| SEC-18 | `cors_origins: ["*"]` default in HTTP gateway | `api_http/server.py:22` | Cross-origin access |
| SEC-19 | No request body size validation in PG wire protocol | `server.py:131-133` | DoS via large message |
| SEC-20 | Checkpoint files written without `umask` control | `checkpoint.py:244` | World-readable secrets |

---

## 3. Data Protection Issues

### 🔴 DP-01: No Encryption at Rest

Data files, WAL segments, checkpoint files (`.qmck`), and ring buffer shared memory are stored in plaintext on disk. Only backup files can optionally be encrypted (AES-256-GCM).

**Recommendation**: Implement tablespace-level transparent data encryption (TDE) or at minimum encrypt checkpoint and WAL files which contain full row data.

---

### 🔴 DP-02: User Catalog (Passwords) Not Persisted to Checkpoint

**File**: `qm_app.py:233-255`

The `_get_checkpoint_state()` method serializes table metadata and Merkle root, but **never serializes the UserCatalog**. On crash recovery, all user accounts except the bootstrap admin are lost.

```python
def _get_checkpoint_state(self) -> dict[str, Any]:
    return {
        "lsn": seq.current_lsn,
        "epoch": seq.epoch,
        "table_meta": table_meta,
        "merkle_root": hub._auditor.root(),
        # ❌ Missing: "user_catalog": self._user_catalog.to_dict()
    }
```

**Fix**: Add `"user_catalog": self._user_catalog.to_dict()` and restore on recovery.

---

### 🟠 DP-03: In-Memory Row Storage — No Durability for Data

Both `QMEngine` and `QMHubEngine` store rows in Python dicts (`dict[int, dict]`). While WAL is written, there is no evidence that WAL replay actually restores row data. The `recover_state()` method only restores metadata (LSN, epoch, schemas), not the actual data.

**Impact**: All data is lost on process restart despite WAL being present.

**Fix**: Implement WAL replay to reconstruct row data, or persist rows to a durable format (LSM-tree, page files).

---

### 🟠 DP-04: Transaction Isolation Issues

**File**: `qm_core/engine.py:266-272`

```python
# WAL
if self._wal:
    self._wal.append(WALOp.INSERT, txn_id=0, ...)

# MVCC write
txn = self._mvcc.begin()
self._mvcc.insert(txn, table, str(doc_id), doc)
self._mvcc.commit(txn)

# Store — happens AFTER MVCC commit
doc["_id"] = doc_id
state.rows[doc_id] = doc
```

- WAL always uses `txn_id=0` — no real transaction tracking
- MVCC transaction begins and commits for every single insert — no multi-statement transactions
- Row store mutation happens after MVCC commit — brief window where reads may see stale data
- No ROLLBACK support

---

### 🟠 DP-05: Delete Doesn't Update Indexes

**File**: `qm_core/engine.py:337-349`

```python
def delete(self, table: str, doc_id: int) -> bool:
    state = self._get_table(table)
    if doc_id not in state.rows:
        return False
    if self._wal:
        self._wal.append(WALOp.DELETE, ...)
    del state.rows[doc_id]
    state.row_count -= 1
    # ❌ No index cleanup: B+tree, inverted index, HNSW, sketches
    return True
```

Deleted rows remain as phantom entries in all indexes (B+tree, inverted index, HNSW, sketches). This leads to:
- Incorrect search results returning deleted documents
- Memory leaks (deleted data referenced in indexes)
- Incorrect cardinality estimates

Same issue exists for `update()` — old index entries are not removed.

---

### 🟡 DP-06: No FOREIGN KEY Enforcement

The schema system defines `ConstraintType.FOREIGN_KEY` and `ColumnSchema.references`, but there's no code that actually validates foreign key references during INSERT/UPDATE/DELETE. The `ConstraintChecker` skips FK validation entirely.

---

### 🟡 DP-07: Shared Memory Ring Buffers Have No Access Control

**File**: `qm_core/ipc/ring_buffer.py`, `qm_engine/src/ipc/ring_buffer.rs`

Ring buffer shared memory files are created with default permissions. Any local user can attach to the shared memory segment and read/write database commands.

---

## 4. Feature Bugs

### 🔴 BUG-01: Hub Engine UPDATE Fetches ALL Rows Before Filtering

**File**: `qm_core/hub_engine.py:618-634`

```python
def _exec_update_sql(self, stmt: UpdateStmt):
    rows = self.find(stmt.table)  # ❌ Fetches ALL rows via IPC
    if stmt.where:
        pred_fn = self._compile_expr(stmt.where)
        for row in rows:
            if pred_fn(row):
                self.update(stmt.table, row.get("_id", 0), updates)
```

For UPDATE/DELETE with WHERE, the hub engine fetches **entire table contents** via IPC, then filters in Python. On a million-row table, this transfers all rows only to discard most of them.

**Same pattern** in `_exec_delete_sql()` (line 636-649).

**Fix**: Push predicates down through IPC to the satellite.

---

### 🔴 BUG-02: Massive Code Duplication Between `QMEngine` and `QMHubEngine`

The expression compiler, join planning, SELECT execution, and all SQL utilities are **copy-pasted** between `engine.py` (1404 lines) and `hub_engine.py` (1174 lines) — roughly 600+ lines of identical code. This means:
- Bug fixes must be applied twice
- Divergence risk is extreme
- Already diverging in some areas (batch insert optimization only in hub_engine)

**Fix**: Extract shared logic into a mixin or base class `_SQLExecMixin`.

---

### 🟠 BUG-03: `_format_bytes()` Loses Precision via Integer Division

**File**: `qm_app.py:578-584`

```python
def _format_bytes(n: int) -> str:
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024:
            return f"{n:.1f} {unit}"
        n /= 1024  # ❌ After first division, n becomes float
    return f"{n:.1f} TB"
```

After the first division `n /= 1024`, `n` becomes a float while still being compared to `1024`. Works but the type annotation declares `int`. Minor.

---

### 🟠 BUG-04: LatchCoupling `read_traverse` Double-Locks

**File**: `qm_core/concurrency.py:140-150`

```python
@contextmanager
def read_traverse(self, parent_id: int, child_id: int):
    parent_lock = self.get_lock(parent_id)
    child_lock = self.get_lock(child_id)
    with parent_lock.read():
        with child_lock.read():
            pass  # Parent released on exit of outer with
    # Now only child is locked  ← ❌ WRONG: both released
    with child_lock.read():    # ← Re-acquires child, creating a gap
        yield
```

The latch coupling implementation is incorrect:
1. Both parent and child locks are released after the inner `with` block
2. Then child is re-acquired — there's a window where neither is held
3. This violates the Lehman-Yao safety property

---

### 🟠 BUG-05: Event Bus Reference in AuditLogger but Not in Engine

The `AuditLogger` subscribes to an `EventBus`, but neither `QMEngine` nor `QMHubEngine` instantiates or fires events to a bus. The audit system is architecturally present but functionally disconnected.

---

### 🟡 BUG-06: Vector Search Returns Wrong Doc Fields in HubEngine

**File**: `qm_core/hub_engine.py:376-377`

```python
results.append({"_id": vid, "_distance": dist, "_score": 1.0 - dist})
```

The hub engine's `vector_search` only returns `_id`, `_distance`, `_score` — it never late-materializes the actual document fields. In contrast, `QMEngine.vector_search()` returns full document copies.

---

### 🟡 BUG-07: HTTP Gateway Engine Dispatch Not Implemented

**File**: `gateway/api_http/server.py:122-123`

```python
async def _dispatch_to_engine(self, engine: str, request: GatewayRequest) -> Any:
    raise NotImplementedError(f"Engine adapter not registered: {engine}")
```

The HTTP gateway exists as a shell but can never execute queries.

---

## 5. SQL Engine — Missing Features

### Supported SQL Currently
| Feature | Status |
|---|---|
| SELECT (basic) | ✅ |
| INSERT INTO ... VALUES | ✅ |
| UPDATE ... SET ... WHERE | ✅ |
| DELETE FROM ... WHERE | ✅ |
| CREATE TABLE | ✅ |
| JOIN (INNER, LEFT, RIGHT, FULL, CROSS) | ✅ |
| GROUP BY / HAVING | ✅ |
| ORDER BY / LIMIT / OFFSET | ✅ |
| Window functions (ROW_NUMBER, RANK, etc.) | ✅ |
| CTE (WITH ... AS) | ✅ |
| DISTINCT | ✅ |
| LIKE / IN / BETWEEN / IS NULL | ✅ |
| Aggregate functions (COUNT, SUM, AVG, MIN, MAX) | ✅ |
| Scalar functions (COALESCE, ABS, UPPER, LOWER, LENGTH) | ✅ |

### Missing SQL Features — Should Be Added

| Priority | Feature | Rationale |
|---|---|---|
| 🔴 P0 | **Parameterized queries** (`$1`, `?`) | Security: prevents SQL injection |
| 🔴 P0 | **ALTER TABLE** (ADD/DROP/RENAME COLUMN) | Basic DDL for schema evolution |
| 🔴 P0 | **DROP TABLE** via SQL | Only available via API, not SQL |
| 🔴 P0 | **BEGIN/COMMIT/ROLLBACK** (real transactions) | Currently no-ops in hub_executor |
| 🟠 P1 | **CREATE INDEX** via SQL | Currently API-only |
| 🟠 P1 | **DROP INDEX** | Not implemented |
| 🟠 P1 | **INSERT ... ON CONFLICT** (UPSERT) | Common Postgres pattern |
| 🟠 P1 | **EXPLAIN ANALYZE** | Query performance debugging |
| 🟠 P1 | **TRUNCATE TABLE** | Fast bulk delete |
| 🟠 P1 | **CREATE USER / DROP USER / ALTER ROLE** via SQL | Auth management from SQL shell |
| 🟠 P1 | **GRANT / REVOKE** | Permission management |
| 🟠 P1 | **Subqueries** (in WHERE, FROM, SELECT) | Common SQL pattern |
| 🟡 P2 | **UNION / INTERSECT / EXCEPT** | Set operations |
| 🟡 P2 | **CASE WHEN** expressions | Conditional logic |
| 🟡 P2 | **CAST / type coercion** | Type conversion |
| 🟡 P2 | **String functions** (CONCAT, SUBSTRING, TRIM, REPLACE) | String manipulation |
| 🟡 P2 | **Date/time functions** (NOW, EXTRACT, DATE_TRUNC) | Temporal queries |
| 🟡 P2 | **Math functions** (CEIL, FLOOR, ROUND, POWER, LOG) | Numeric operations |
| 🟡 P2 | **EXISTS** subquery | Existence checks |
| 🟡 P2 | **CREATE TABLE AS SELECT (CTAS)** | Table cloning |
| 🟡 P2 | **INSERT INTO ... SELECT** | Bulk insert from query |
| 🟡 P2 | **Views** (CREATE VIEW / DROP VIEW) | Logical views |
| 🟡 P2 | **RETURNING** clause | Return affected rows |
| ⚪ P3 | **Triggers** (CREATE TRIGGER) | Event-driven logic |
| ⚪ P3 | **JSON operators** (`->`, `->>`, `@>`) | JSON querying |
| ⚪ P3 | **Array operators** | Array column support |
| ⚪ P3 | **Lateral joins** | Correlated subqueries |
| ⚪ P3 | **Materialized views** | Precomputed views |
| ⚪ P3 | **Sequences** (CREATE SEQUENCE, NEXTVAL) | Auto-increment |
| ⚪ P3 | **Information schema** | System catalog queries |

---

## 6. CLI Bugs & Issues

### 🟠 CLI-01: `qm sql` Passthrough Has No Error Context

**File**: `tools/qm_cli.py:312-321`

```python
def cmd_sql(args):
    engine = qm_engine.NativeSqlEngine(args.data_dir)
    try:
        cols, rows, tag = engine.execute(args.query)
    except Exception as e:
        print(f"error: {e}", file=sys.stderr)
        sys.exit(1)
```

No indication of which SQL statement failed when running multi-statement scripts. No query echo. No timing information.

---

### 🟠 CLI-02: `qm dump --format sql` Generates Non-Importable SQL

**File**: `tools/qm_cli.py:284-295`

The SQL dump format doesn't include:
- `CREATE TABLE` statements
- Schema type information
- Transaction wrapping (BEGIN/COMMIT)
- DROP TABLE IF EXISTS

This makes the dump unusable for database restoration via SQL.

---

### 🟡 CLI-03: `qm dump` Opens File Without Context Manager Properly

**File**: `tools/qm_cli.py:269-303`

```python
out = open(args.output, "w", ...) if args.output else sys.stdout
try:
    # ... write ...
finally:
    if args.output:
        out.close()  # ❌ File handle leaked on exception before try
```

If `open()` fails between assignment and `try`, the file isn't properly cleaned up. Should use `contextlib.ExitStack`.

---

### 🟡 CLI-04: Version Mismatch Between CLI Tools

- `tools/qm_cli.py` reports `v2.0.0`
- `qm_app.py` reports `v1.0.0` (`__version__`)
- `QMHubEngine.VERSION` reports `2.0.0-hub`
- `QMEngine.VERSION` reports `1.0.0`

No single source of truth for version.

---

### 🟡 CLI-05: `qmvir start` Legacy Flags Conflict with Subcommand Args

**File**: `qm_app.py:892-903`

Legacy global flags (`--start`, `--status`, `--stop`, `--host`, `--port`) are registered at the parser root level alongside the subcommand parser. This creates arg namespace collisions — e.g., `--host` on root vs `--host` on `start` subcommand.

---

### 🟡 CLI-06: Missing `qm compact` Command

Compaction is implemented (`CompactionEngine`) but not exposed via CLI. No way for operators to trigger manual compaction.

---

### 🟡 CLI-07: `qm check` Only Counts Rows — No Real Integrity Check

**File**: `tools/qm_cli.py:236-249`

The "check" command only runs `SELECT COUNT(*) FROM table`. It doesn't verify:
- Index consistency (B+tree, HNSW, inverted index)
- WAL integrity
- Checkpoint validity
- Foreign key referential integrity
- Page-level checksums

---

## 7. Architecture Evaluation

### 7.1 Strengths

#### ✅ Hub-Satellite IPC Architecture
The separation of control plane (Hub: SQL parsing, planning, metadata) and data plane (Satellites: storage, indexing) via ring buffer IPC is a sophisticated design inspired by production databases. Benefits:
- Process isolation prevents satellite crashes from taking down the hub
- Each satellite can be scaled independently
- Ring buffers provide zero-copy, lock-free communication

#### ✅ Dual Engine (Python + Rust)
The auto-fallback from Rust to Python ensures the system always works:
- `qm_engine` (Rust/PyO3) for production performance
- `QMHubEngine` (Python) for rapid prototyping and testing
- `native_adapter.py` provides clean abstraction layer

#### ✅ Comprehensive Index Support
- B+tree with Lehman-Yao latch coupling
- HNSW for approximate nearest neighbour search
- Product Quantizer for vector compression
- Inverted index with BM25 and Block-Max WAND
- Roaring bitmap indexes
- Composite and hash indexes

#### ✅ Strong Observability
- Merkle tree auditing for data integrity
- Metrics and tracing modules
- Slow query logging
- Index health monitoring
- Chaos testing framework

#### ✅ Adaptive Query Optimization
- Learned selectivity estimation
- Plan history for regression detection
- Rule-based optimizer
- Cost model V2 with catalog statistics

### 7.2 Weaknesses

#### ❌ In-Memory Only Storage (Python Path)
All row data lives in Python dicts. No page-based storage, no memory-mapped files, no buffer pool integration for the Python engine. Only the Rust `StorageEngine` has real persistent storage.

#### ❌ No Real Transaction Support
- `txn_id=0` everywhere
- BEGIN/COMMIT/ROLLBACK are no-ops
- No write-ahead log for transaction recovery
- MVCC is used but committed immediately per operation

#### ❌ Massive Code Duplication
~600 lines duplicated between `engine.py` and `hub_engine.py`. The expression compiler, SQL execution, join planning, and all operator infrastructure is copied.

#### ❌ Tight Coupling Between Gateway and Engine
The hub executor directly calls `engine.execute_sql()`, `engine.vector_search()`, etc. There's no query routing abstraction that would allow distributed query execution across multiple nodes.

#### ❌ No Connection Pooling
Each TCP connection gets direct access to the engine. No connection pool, no prepared statement cache, no session management.

### 7.3 Architecture Diagram

```
┌──────────────────────────────────────────────────────────┐
│                    QM Daemon (qm_app.py)                 │
│  ┌─────────┐  ┌───────────┐  ┌──────────┐  ┌─────────┐ │
│  │  Auth    │  │  Health   │  │Checkpoint│  │  Logs   │ │
│  │ Catalog  │  │  Monitor  │  │ Manager  │  │ Layered │ │
│  └────┬─────┘  └─────┬─────┘  └─────┬────┘  └─────────┘ │
│       │               │              │                    │
│  ┌────▼────────────────▼──────────────▼──────────┐       │
│  │              QMHubEngine (control plane)       │       │
│  │  SQL Parser → Planner → Optimizer → Executor  │       │
│  └────────────────────┬──────────────────────────┘       │
│                       │ HubDispatcher                     │
│           ┌───────────┼───────────┐                      │
│  ┌────────▼──┐ ┌──────▼──┐ ┌─────▼─────┐                │
│  │ Ring Gen  │ │Ring Vec │ │ Ring Proc  │                 │
│  └─────┬─────┘ └────┬────┘ └─────┬─────┘                │
│  ┌─────▼─────┐ ┌────▼────┐ ┌─────▼─────┐                │
│  │  General  │ │ Vector  │ │ Procedure │  (Satellites)   │
│  │ Satellite │ │Satellite│ │ Satellite │                 │
│  │ (Storage, │ │ (HNSW,  │ │  (PL/QM)  │                │
│  │  Index)   │ │ ANN)    │ │           │                 │
│  └───────────┘ └─────────┘ └───────────┘                 │
│                                                          │
│  ┌──────────────────────────────────────────────────┐    │
│  │       Gateway (PostgreSQL Wire Protocol)         │    │
│  │  Python (asyncio) ←→ Rust (tokio) [preferred]    │    │
│  └──────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────┘
```

### 7.4 Module Coupling Analysis

| Module | Fan-out | Assessment |
|---|---|---|
| `qm_core/engine.py` | 22 imports | ⚠️ God class — too many responsibilities |
| `qm_core/hub_engine.py` | 24 imports | ⚠️ Same issue + duplication |
| `qm_app.py` | 14 imports | Acceptable for daemon bootstrap |
| `qm_core/auth.py` | 4 imports | ✅ Clean, minimal deps |
| `qm_core/checkpoint.py` | 5 imports | ✅ Clean |
| `gateway/api_postgres/` | 3 imports | ✅ Clean |

### 7.5 Test Coverage Observations

- 37 test files with comprehensive unit + integration tests
- Chaos testing framework (`qmvir_stress_chaos.py`)
- Benchmarks against PostgreSQL and DuckDB
- **Missing**: No auth/security tests, no fuzz testing for SQL parser, no TLS tests

---

## 8. Recommendations Priority Matrix

### P0 — Must Fix Before Production

| # | Issue | Effort | Impact |
|---|---|---|---|
| 1 | SEC-03: Add TLS/SSL support | High | Compliance blocker |
| 2 | SEC-04: Enforce authentication on PG wire | Medium | Complete bypass |
| 3 | SEC-05: Wire RBAC into data path | Medium | Privilege escalation |
| 4 | SEC-02: Remove default admin password | Low | Trivial exploitation |
| 5 | DP-02: Persist user catalog to checkpoint | Low | Data loss on crash |
| 6 | SEC-01: Parameterized queries / identifier escaping | Medium | SQL injection |
| 7 | SEC-12: Path traversal in LINK MEDIA | Low | Arbitrary file read |

### P1 — Fix in Next Release

| # | Issue | Effort | Impact |
|---|---|---|---|
| 8 | SEC-06: Audit all `unsafe` Rust blocks | High | Memory safety |
| 9 | SEC-07: Fix LIKE regex injection | Low | Query crash/DoS |
| 10 | DP-05: Fix delete/update index cleanup | Medium | Data correctness |
| 11 | BUG-01: Push predicates down in UPDATE/DELETE | Medium | Performance |
| 12 | BUG-02: Extract shared SQL executor code | Medium | Maintainability |
| 13 | SEC-09: Add auth rate limiting | Low | Brute force protection |
| 14 | Add real transaction support (BEGIN/COMMIT/ROLLBACK) | High | Data integrity |

### P2 — Improve

| # | Issue | Effort | Impact |
|---|---|---|---|
| 15 | Add SQL EXPLAIN/ANALYZE | Medium | Debuggability |
| 16 | Add ALTER TABLE support | Medium | Schema evolution |
| 17 | CLI version consistency | Low | User confusion |
| 18 | Implement FK enforcement | Medium | Data integrity |
| 19 | Add connection pooling | Medium | Scalability |
| 20 | Durable row storage (Python path) | High | Data persistence |

---

## Appendix A: Files Audited

```
qm_app.py                          — Daemon + CLI entry point (1025 lines)
qm_core/engine.py                  — Standalone QM engine (1404 lines)
qm_core/hub_engine.py              — Hub-Satellite engine (1174 lines)
qm_core/auth.py                    — RBAC authentication (299 lines)
qm_core/audit.py                   — Audit logging (143 lines)
qm_core/checkpoint.py              — Checkpoint manager (341 lines)
qm_core/concurrency.py             — Locks and concurrency (236 lines)
qm_core/schema.py                  — Schema & constraints (317 lines)
qm_core/native_adapter.py          — Rust/Python adapter (340 lines)
tools/qm_cli.py                    — CLI tool (421 lines)
gateway/api_postgres/server.py      — PG TCP server (188 lines)
gateway/api_postgres/hub_executor.py — SQL executor (227 lines)
gateway/api_http/server.py          — HTTP gateway (144 lines)
gateway/auth/acl.py                 — ACL system (89 lines)
qm_native_c/qm_native_c.c          — SIMD vector ops (255 lines)
qm_engine/src/lib.rs                — Rust module root (76 lines)
qm_engine/src/types.rs              — Zero-copy types (338 lines)
Dockerfile                          — Container image (56 lines)
scripts/build_native.sh             — Build script (48 lines)
+ grep analysis across entire codebase
```

---

*End of Audit Report*
