# QMvir — Basic Usage Guide

**Version:** 6.2.0  
**Binary:** `qm` / `qmvir` (Rust, no Python runtime required)  
**Protocol:** PostgreSQL wire v3 — `psql`, JDBC, any PG client  
**Default port:** `55433`

---

## 1. Install

### npm (recommended)

```bash
npm install -g qmvir
qm --version    # qm 6.2.0
```

Postinstall downloads the native binary for your OS/arch from [GitHub releases](https://github.com/virgori/qmvir-releases).

### Direct download

```bash
# Example: Linux x86_64
curl -LO https://github.com/virgori/qmvir-releases/releases/download/v6.2.0/qm-linux-x86_64
chmod +x qm-linux-x86_64 && sudo mv qm-linux-x86_64 /usr/local/bin/qm
```

Supported artifacts: `qm-macos-arm64`, `qm-macos-x86_64`, `qm-linux-x86_64`, `qm-linux-aarch64`, `qm-windows-x86_64.exe`, `qm-windows-aarch64.exe`.

### Build from source

```bash
cd qm_engine
cargo build --release --no-default-features --bin qm
./target/release/qm --version
```

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

Hybrid lexical + vector search is available via SQL extensions and the JS SDK (`strategy: "hybrid"`).

---

## 5. Vector search

```sql
CREATE TABLE items (id INT PRIMARY KEY, vec VECTOR(384));
CREATE INDEX idx_items_hnsw ON items USING HNSW(vec);

INSERT INTO items VALUES (1, '[0.01, 0.02, ...]');
SELECT id, distance(vec, '[0.01, 0.02, ...]') AS dist
FROM items ORDER BY dist LIMIT 10;
```

HNSW + optional product quantization — see [Algorithms](QMVIR_ALGORITHMS.md).

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

## 8. JavaScript / TypeScript SDK

```typescript
import { QMClient } from "qmvir";

const qm = new QMClient("http://localhost:8400", { apiKey: "your-key" });
const rows = await qm.find("users", { where: { status: "active" }, limit: 10 });
const hits = await qm.search("articles", "machine learning", { strategy: "hybrid", limit: 20 });
```

Install: `npm install qmvir@6.2.0`

---

## 9. Useful CLI commands

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

## 10. Configuration highlights

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
| Architecture | [QMVIR_ARCHITECTURE.md](QMVIR_ARCHITECTURE.md) |
| Algorithms | [QMVIR_ALGORITHMS.md](QMVIR_ALGORITHMS.md) |
| Enterprise HA / multi-DC | [ENTERPRISE_HA_GUIDE.md](ENTERPRISE_HA_GUIDE.md) |
| Enterprise (Tiếng Việt) | [ENTERPRISE_HA_GUIDE_VI.md](ENTERPRISE_HA_GUIDE_VI.md) |
