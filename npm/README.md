# qmvir

**QMvir v6.2.3** — Hybrid AI-native database: OLTP + OLAP + full-text search + vector search in one Rust engine. PostgreSQL wire protocol. **Production Multi-DC Enterprise Certified** (optional HA cluster).

## What's new in 6.2.3

- Fix PG wire `new_session()` registration so `node-pg` connection pools can INSERT/UPDATE/DELETE (was `Session not registered` on connection 2+)

## What's new in 6.2.2

- npm README sync with published release docs

## What's new in 6.2.1

- Faster `LIKE '%…%'` id-only scans (packed int64 + columnar bridge)
- Release pipeline fix: npm `postinstall` downloads GitHub binaries matching `package.json` version (no stale 6.2.0 bins)

## Install

```bash
npm install qmvir@6.2.3
# or global CLI
npm install -g qmvir
qm --version   # qm 6.2.3
```

Postinstall downloads the native binary (~8–13 MB) for **your** platform from [GitHub releases](https://github.com/virgori/qmvir-releases/releases/tag/v6.2.3).

Linux and Windows binaries are **not** bundled in the npm package (that would ship ~70 MB for all platforms). After `npm install`, `scripts/postinstall.js` fetches exactly one binary:

| Platform | Downloaded asset |
|----------|------------------|
| macOS Apple Silicon | `qm-macos-arm64` |
| macOS Intel | `qm-macos-x86_64` |
| Linux x86_64 | `qm-linux-x86_64` |
| Linux ARM64 | `qm-linux-aarch64` |
| Windows x86_64 | `qm-windows-x86_64.exe` |
| Windows ARM64 | `qm-windows-aarch64.exe` |

If a cached binary reports a different version, postinstall re-downloads automatically.

## Quick start

```bash
qm --data-dir ./data start --admin-password secret
psql -h 127.0.0.1 -p 55433 -U admin -d qm
```

```sql
CREATE TABLE users (id INT PRIMARY KEY, name TEXT);
INSERT INTO users VALUES (1, 'Alice');
SELECT * FROM users;
```

## SDK (JavaScript / TypeScript)

```typescript
import { QMClient } from "qmvir";

const qm = new QMClient("http://localhost:8400", { apiKey: "your-key" });
const rows = await qm.find("users", { where: { status: "active" }, limit: 10 });
const hits = await qm.search("articles", "machine learning", { strategy: "hybrid", limit: 20 });
```

## CLI highlights

```bash
qm --data-dir ./data backup -o backup.qmvb
qm --data-dir ./data inspect --tables
qm cluster status              # when QM_CLUSTER_ENABLE=1
qm cluster certify             # enterprise HA certification
qm cluster guide               # HA setup walkthrough
qm guide quickstart
```

## Platforms

| Platform | Binary |
|----------|--------|
| macOS Apple Silicon | `qm-macos-arm64` |
| macOS Intel | `qm-macos-x86_64` |
| Linux x86_64 | `qm-linux-x86_64` |
| Linux ARM64 | `qm-linux-aarch64` |
| Windows x86_64 | `qm-windows-x86_64.exe` |
| Windows ARM64 | `qm-windows-aarch64.exe` |

## Documentation

Public install and release docs: [virgori/qmvir-releases](https://github.com/virgori/qmvir-releases).

Full architecture docs ship with the private source repository for collaborators.

## License

See [LICENSE](LICENSE) in this package.
