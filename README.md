# QMvir - Hybrid AI-Native Database Engine

<div align="center">

[![License: Proprietary](https://img.shields.io/badge/License-Proprietary-red.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-blue.svg)]()

**Rust-first hybrid database engine combining SQL, analytics-oriented execution, full-text search, and vector search in one process**

[Quick Start](#quick-start) • [Features](#features) • [Performance](#performance) • [Documentation](#documentation)

</div>

---

## Overview

QMvir is a **Rust-first hybrid database engine**. The active release surface is the `qm_engine` crate: the `qm` CLI, PostgreSQL wire-protocol gateway, native SQL execution, WAL/checkpoint persistence, backup/restore, full-text search, vector search, a local web dashboard, and opt-in HA/cluster modules.

**v6.2.8** fixes extended-query reads (Bind param substitution, RowDescription) and speeds OLTP prepared DML via per-table autocommit paths and prepared-plan caching. **v6.2.7** fixes UTF-8 panic in UPDATE keyword scan (accents/CJK). **v6.2.6** fixed WHERE-in-literal hang and soft checkpoint. **v6.2.5** fixed HTAP write-lock deadlock. **v6.2.4** deferred checkpoint / group-commit defaults.

**v6.2.0** added optional HA/cluster machinery: sync WAL replication, automatic failover, meta Raft, 2PC, STONITH, witness, and `qm cluster certify` certification tiers. These paths are opt-in and should be validated in the target deployment before production claims.

### What Makes QMvir Special?

- **Hybrid core**: SQL, HTAP-style row/column paths, full-text search, and vector index modules live in one Rust crate.
- **PostgreSQL gateway**: the server speaks PostgreSQL wire protocol for the supported SQL surface.
- **Native persistence**: `NativeSqlEngine` uses per-table snapshots plus an append-only SQL WAL.
- **Search and vectors**: inverted indexes and HNSW/PQ vector search are implemented natively, with fallback paths where needed.
- **Operational tooling**: CLI commands cover start/stop, SQL execution, backup/restore, inspection, HA checks, and local dashboard access.
- **Explicit R&D boundary**: JIT native-code execution, adaptive indexing policy, IPC hub/satellite routing, CDC streaming, and multi-DC HA are retained as opt-in or experimental surfaces until validated per workload.

---

## Quick Start

### Installation

#### Build from Source
```bash
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features
./qm_engine/target/release/qm --data-dir ./data start --admin-password your-secure-password
```

### Basic Usage

```bash
# Start QMvir server
qm --data-dir ./data start --admin-password your-secure-password

# Connect with psql (default port 55433)
psql -h localhost -p 55433 -U admin -d qm

# Or use any PostgreSQL-compatible client
```

---

## Features

### Current Release Surface
- **Native SQL engine**: SQL DDL/DML/query execution, WAL/checkpoint recovery, and PostgreSQL wire integration.
- **Transaction scope**: gateway-native transactions support `BEGIN`, `COMMIT`, and `ROLLBACK` with READ COMMITTED semantics. Serializable and savepoint semantics are not claimed for the native gateway path.
- **HTAP / analytics modules**: durable column segments, vectorized helpers, and planner hooks support analytics-oriented execution paths.
- **Search engine**: full-text search uses native inverted-index modules with WAND/BMW-style retrieval support.
- **Vector engine**: HNSW indexes with product-quantization support provide approximate nearest-neighbor search.
- **Backup / restore**: `.qmvb` backup format, verification, encryption support, and PITR-related metadata paths.
- **Web dashboard**: `qm_web` exposes a loopback-only Axum dashboard and REST API for local administration.

### Performance-Oriented Building Blocks
- **SIMD Acceleration**: AVX2/NEON vectorized operations
- **Parallel Processing**: Rayon thread pool for CPU-intensive tasks
- **Memory Mapping**: Efficient data access with mmap-based storage
- **JIT Infrastructure**: expression IR, cache, and vectorized interpretation; native machine-code JIT should be treated as experimental until wired and benchmarked end-to-end
- **Adaptive Indexing**: index-observer and auto-manager modules exist; deployment policy should be validated per workload

### Compatibility & Integration
- **PostgreSQL Wire Protocol**: Client compatibility for the supported gateway surface
- **Standard SQL Subset**: ANSI-style SQL support with QMvir extensions and documented gaps
- **Local REST Dashboard**: Axum-based admin API in `qm_web`
- **Optional Python Bridge**: PyO3 bindings are behind the `python` feature; default Rust builds use `--no-default-features`
- **Export / Import Formats**: CLI dump supports SQL, CSV, JSON Lines, and Parquet; native SQL supports Parquet import and wire `COPY FROM STDIN`
- **CDC / Streaming Hooks**: CDC and WAL streaming modules exist, but broad “real-time ingestion platform” claims should remain roadmap-level until validated

---

## Performance

### Benchmark Notes

Benchmarks must be interpreted by mode. QMvir has fast local/in-memory scalar read and index paths. Persistent WAL `per_commit_sync` has stricter durability-at-return semantics but durable mutating workloads may be slower than PostgreSQL depending on workload and fsync policy. Group commit can improve throughput, but it must not be treated as PostgreSQL `synchronous_commit=on` if mutations are acknowledged before fsync.

```bash
cargo bench --manifest-path qm_engine/Cargo.toml
cargo run --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin native_sql_core_bench
cargo run --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin hnsw_rust_benchmark
```

---

## Architecture

The current runtime is centered on `PostgresGateway` and `NativeSqlEngine`; HTAP, search, vector, backup, web, and cluster modules live in the same Rust crate:

```
┌─────────────────────────────────────────────────────────────┐
│                    Client Applications                      │
├─────────────────────────────────────────────────────────────┤
│  PostgreSQL Clients  │      qm CLI      │  qm_web local API │
└──────────────────────┴──────────────────┴───────────────────┘
                              │
                    ┌─────────▼─────────┐
                    │  PostgresGateway  │  ← Wire Protocol v3
                    │  (Tokio TCP)      │
                    └─────────┬─────────┘
                              │
                    ┌─────────▼─────────┐
                    │ NativeSqlEngine   │  ← SQL execution
                    │   SIMD + Cache    │
                    └─────────┬─────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
┌─────────────┐    ┌───────────────┐    ┌─────────────────┐
│   HTAP      │    │   Search      │    │    Vector       │
│ row/column  │    │ FTS+Inverted  │    │   HNSW+PQ       │
└─────────────┘    └───────────────┘    └─────────────────┘
                              │
                    ┌─────────▼─────────┐
                    │   Storage Layer   │
                    │ WAL + Snapshots   │
                    │ backup/PITR meta  │
                    └───────────────────┘
```

---

## Documentation

| # | Guide | Description |
|---|--------|-------------|
| 1 | [Architecture](docs/en/QMVIR_ARCHITECTURE.md) | Gateway, engine, storage, HA boundaries |
| 2 | [Algorithms](docs/vi/QMVIR_ALGORITHMS.md) | Data structures & algorithms (Vietnamese, with source map) |
| 3 | [Basic usage](docs/en/BASIC_USAGE.md) | Install, SQL, search, vector, backup, CLI |
| 4 | [Enterprise HA](docs/en/ENTERPRISE_HA_GUIDE.md) | Multi-DC deployment, certification, env vars |

**Index:** [docs/README.md](docs/README.md) · **Tiếng Việt:** [docs/vi/](docs/vi/)

Historical Python-era reports and old audits were removed from the active tree.

### Links

- **Source:** private repo `virgori/qmvir-db` (collaborators only)

---

## Development

### Building from Source

```bash
# Prerequisites
rustc 1.75+
clang 14+
libssl-dev

# Build release version
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features

# Run tests
cargo test --manifest-path qm_engine/Cargo.toml

# Run benchmarks
cargo bench --manifest-path qm_engine/Cargo.toml
```

### Project Structure

```
QM/
├── qm_engine/           # Rust core engine
│   ├── src/
│   │   ├── gateway/     # PostgreSQL wire protocol
│   │   ├── storage/     # Persistence & WAL
│   │   ├── index/       # Multi-modal indexing
│   │   ├── executor/    # Query execution, SIMD, JIT infrastructure
│   │   ├── htap/        # Row/column runtime, MVCC helpers, PITR
│   │   ├── cluster/     # Opt-in HA / multi-node features
│   │   └── web/         # Local dashboard/API
│   └── Cargo.toml
├── docs/               # Current docs grouped by language
│   ├── en/             # English guides
│   ├── vi/             # Vietnamese guides
│   └── internal/       # Maintainer handover notes
├── scripts/            # Release, HA, and remote build helpers
└── tests/              # Small PyO3 smoke suite
```

---

## Community & Support

- **Issues / discussions:** contact maintainers (source repo is private)
- **Documentation**: [docs/README.md](docs/README.md)

---

## License

QMvir is distributed under the proprietary license in [LICENSE](LICENSE). See that file for usage and distribution terms.

---

<div align="center">

**[Read the docs](docs/README.md) • [Get started now](#quick-start)**

Made by the QMvir Team

</div>
