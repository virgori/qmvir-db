# qmvir

**QMvir v4.3.4** — Hybrid AI-Native Database combining OLTP + OLAP + Full-text Search + Vector Search + Smart Cache in one platform. Pure Rust engine, zero Python dependency.

## Install

```bash
# npm
npm install qmvir

# pnpm
pnpm add qmvir

# Global CLI (auto-downloads native Rust binary)
npm install -g qmvir
```

The package auto-downloads the correct native Rust binary (~7–8 MB) for your platform during `postinstall`.

## SDK Usage (JavaScript / TypeScript)

```typescript
import { QMClient } from "qmvir";

const qm = new QMClient("http://localhost:8400", { apiKey: "your-key" });

// CRUD
const users = await qm.find("users", {
  where: { status: "active" },
  orderBy: [{ created_at: "desc" }],
  limit: 10,
});

await qm.insert("users", { name: "Alice", balance: 150.5 });
await qm.update("users", "user-123", { balance: 200.0 });
await qm.delete("users", "user-456");

// Full-text + Vector hybrid search
const results = await qm.search("articles", "machine learning", {
  strategy: "hybrid", // lexical | vector | hybrid | rerank
  limit: 20,
});

// Analytics aggregation
const stats = await qm.aggregate("orders", {
  groupBy: ["region", "category"],
  metrics: [{ total: "SUM(amount)" }, { count: "COUNT(*)" }],
  where: { year: 2026 },
});
```

## CLI Usage

```bash
# Start QMvir server (PostgreSQL wire protocol)
qmvir --data-dir ./data start --host 127.0.0.1 --port 55433

# Connect via psql
psql -h 127.0.0.1 -p 55433 -U admin -d qm

# SQL queries
CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, balance DOUBLE PRECISION);
INSERT INTO users VALUES (1, 'Alice', 150.50);
SELECT * FROM users WHERE balance > 100;

# Backup & Restore
qmvir --data-dir ./data backup -o backup.qmvb
qmvir --data-dir ./data restore backup.qmvb

# Inspect
qmvir --data-dir ./data inspect --tables
qmvir --data-dir ./data stat --json
```

## Benchmark Results (v4.3.4, VPS Contabo 24GB RAM, Linux x86_64)

| Benchmark | ops/s | Time |
|-----------|------:|-----:|
| Ring Buffer IPC (1KB) | **266.2K** | 15.4 ms |
| LSN Sequencer | **190.48M** | 525 μs |
| B+Tree point lookup | **6.30M** | 1.6 ms |
| Roaring Bitmap contains | **416.67M** | 24 μs |
| Bloom Filter lookup | **2.50G** | 4 μs |
| HNSW search top-10 (128d) | **30.4K** | 32.9 ms |
| SQL SELECT by PK (e2e) | **753.8K** | 6.6 ms |
| SQL INSERT (e2e) | **1.1K** | 9.39 s |
| SQL COUNT(\*) aggregation | **574.7K** | 174 μs |

**Linux production features:** io_uring WAL, AVX-512F SIMD, O_DIRECT, madvise, fdatasync.

## Supported Platforms

| Platform | Architecture | Binary | Size |
| -------- | ------------ | ------ | ---- |
| macOS | ARM64 (M1+) | `qm-macos-arm64` | 7.4 MB |
| Linux | x86_64 | `qm-linux-x86_64` | 8.4 MB |
| Linux | ARM64 | `qm-linux-aarch64` | 6.9 MB |
| Windows | x86_64 | `qm-windows-x86_64.exe` | 8.2 MB |

## Alternative Install

```bash
# Shell script (auto-detect platform)
curl -fsSL https://raw.githubusercontent.com/virgori/qmvir-releases/main/install.sh | bash
```

## License

MIT
