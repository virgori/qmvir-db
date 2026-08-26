# QMvir — Basic Usage Guide

**Version:** 6.2.8  
**Binary:** `qm` / `qmvir` (Rust, no Python runtime required)  
**Protocol:** PostgreSQL wire v3 — `psql`, JDBC, any PG client  
**Default port:** `55433`

---

## 1. Install

### Build from source

```bash
cd qm_engine
cargo build --release --no-default-features --bin qm
./target/release/qm --version
```

Release binaries are built by GitHub Actions from the private source repo. Run `.github/workflows/release-binaries.yml` manually or push a `v*` tag to produce Linux, macOS, and Windows artifacts without compiling on the local Mac.

---

## 2. Quick start

```bash
# Start server (daemon)
qm --data-dir ./mydb start --admin-password secret

# Foreground (logs to terminal)
qm --data-dir ./mydb start --admin-password secret --foreground

# Connect
psql -h 127.0.0.1 -p 55433 -U admin -d qm
```

Interactive CLI:

```bash
qm --data-dir ./mydb sql "SELECT 1"
qm --data-dir ./mydb inspect --tables
qm guide quickstart          # built-in guide
```

---

## 3. SQL essentials

```sql
CREATE TABLE users (
  id INTEGER PRIMARY KEY,
  name TEXT,
  balance DOUBLE PRECISION,
  embedding VECTOR(128)
);

INSERT INTO users VALUES (1, 'Alice', 100.0, '[0.1,0.2,...]');
SELECT * FROM users WHERE id = 1;
UPDATE users SET balance = 150 WHERE id = 1;
DELETE FROM users WHERE id = 1;
```

**Transaction note:** `BEGIN`/`COMMIT` are accepted on the wire but native engine uses **auto-commit per statement** unless cluster distributed txn is enabled (see [Enterprise guide](ENTERPRISE_HA_GUIDE.md)).

---

## 4. Full-text search

```sql
CREATE TABLE docs (id INT PRIMARY KEY, title TEXT, body TEXT);
CREATE INDEX idx_docs_fts ON docs USING FTS(title, body);

INSERT INTO docs VALUES (1, 'Rust database', 'QMvir hybrid engine');
SELECT * FROM docs WHERE body @@ 'database';
```

Hybrid lexical + vector search is available via SQL extensions.

---

## 5. Vector search

```sql
CREATE TABLE items (id INT PRIMARY KEY, vec VECTOR(384));
CREATE INDEX idx_items_hnsw ON items USING HNSW(vec);

INSERT INTO items VALUES (1, '[0.01, 0.02, ...]');
SELECT id, distance(vec, '[0.01, 0.02, ...]') AS dist
FROM items ORDER BY dist LIMIT 10;
```

HNSW + optional product quantization — see [Algorithms](../vi/QMVIR_ALGORITHMS.md).

---

## 6. Backup & restore

```bash
qm --data-dir ./mydb backup -o backup.qmvb
qm --data-dir ./mydb restore backup.qmvb
qm --data-dir ./mydb inspect --tables
```

Backups support encryption (AES-GCM) when configured.

---

## 7. Web dashboard & REST

```bash
# Optional HTTP dashboard (separate binary or flags — see qm guide)
qm --data-dir ./mydb start --admin-password secret
# Default REST/WebSocket on configured port (see `qm guide`)
```

---

## 8. Useful CLI commands

| Command | Purpose |
|---------|---------|
| `qm start` | Start PG gateway + engines |
| `qm stop` / `qm status` | Process control |
| `qm sql "..."` | One-shot SQL |
| `qm backup` / `qm restore` | Snapshot backup |
| `qm inspect --tables` | Schema listing |
| `qm stat --json` | Runtime metrics |
| `qm cluster status` | HA cluster (when enabled) |
| `qm cluster certify` | Enterprise certification |
| `qm cluster guide` | HA setup walkthrough |
| `qm guide` | General product guide |

---

## 9. Configuration highlights

| Variable | Effect |
|----------|--------|
| `QM_DATA_DIR` | Data directory (or `--data-dir`) |
| `QM_ADMIN_PASSWORD` | Admin SCRAM password |
| `QM_ANALYTICS_ENGINE=0` | Disable analytics worker pool |
| `QM_VECTOR_ENGINE=0` | Disable vector worker pool |

Cluster / HA variables: see [Enterprise HA Guide](ENTERPRISE_HA_GUIDE.md).

---

## Next steps

| Topic | Document |
|-------|----------|
| Architecture | [QMVIR_ARCHITECTURE.md](../vi/QMVIR_ARCHITECTURE.md) |
| Algorithms | [QMVIR_ALGORITHMS.md](../vi/QMVIR_ALGORITHMS.md) |
| Enterprise HA / multi-DC | [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md) |
| Enterprise (Tiếng Việt) | [ENTERPRISE_HA_GUIDE.md](../vi/ENTERPRISE_HA_GUIDE.md) |
