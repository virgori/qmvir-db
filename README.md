# 🚀 QMvir - Hybrid AI-Native Database Engine

<div align="center">

[![npm version](https://badge.fury.io/js/qmvir.svg)](https://badge.fury.io/js/qmvir)
[![License: Proprietary](https://img.shields.io/badge/License-Proprietary-red.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-blue.svg)]()

**High-performance hybrid database combining OLTP, OLAP, Full-Text Search, and Vector Search in a single engine**

[Quick Start](#-quick-start) • [Features](#-features) • [Performance](#-performance) • [Documentation](#-documentation)

</div>

---

## 🎯 Overview

QMvir is a **hybrid database engine** (Rust) combining transactional SQL, analytics-style execution, full-text search, and vector similarity in one process. It exposes a **PostgreSQL wire-protocol** gateway for standard clients.

**v6.2.6** fixes UPDATE hang when values contain `WHERE`, and makes checkpoint skip large-table clones so OLTP never stalls. **v6.2.5** fixed HTAP write-lock deadlock. **v6.2.4** deferred checkpoint / group-commit defaults. **v6.2.3** fixed `node-pg` pool session registration.

**v6.2.0** added optional **Production Multi-DC Enterprise HA**: sync WAL replication, automatic failover, meta Raft, 2PC, STONITH, witness, and `qm cluster certify` certification tiers.

### 🌟 What Makes QMvir Special?

- **🔥 Hybrid Architecture**: OLTP + OLAP + Search + Vector in one engine
- **⚡ Lightning Fast**: Rust-powered with SIMD optimizations and parallel processing
- **🔌 PostgreSQL Wire Protocol**: Compatible with common PostgreSQL clients for supported SQL/features
- **🧠 AI-Native**: Built-in vector search with HNSW and Product Quantization
- **🔍 Full-Text Search**: Advanced inverted index with WAND/BMW optimization
- **📊 Real-time Analytics**: Columnar storage with vectorized execution
- **🛡️ Release-Surface Focus**: WAL/checkpoint recovery, backups, security, and monitoring with documented limitations

---

## 🚀 Quick Start

### Installation

#### Option 1: NPM (Recommended)
```bash
npm install -g qmvir@6.2.6
qm --version
```

#### Option 2: Build from Source
```bash
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features
./qm_engine/target/release/qm --data-dir ./data start --admin-password your-secure-password
```

#### Option 3: Docker
```bash
docker run -p 5433:5433 virgori/qmvir:latest
```

### Basic Usage

```bash
# Start QMvir server
qm --data-dir ./data start --admin-password your-secure-password

# Connect with psql (default port 5433)
psql -h localhost -p 5433 -U admin -d qmvir

# Or use any PostgreSQL-compatible client
```

---

## ✨ Features

### 🏗️ Hybrid Architecture
- **OLTP Engine**: SQL transactions with WAL/checkpoint recovery; full storage-wide MVCC remains scoped to the storage/MVCC modules and is not yet a blanket NativeSqlEngine claim
- **OLAP Engine**: Columnar storage with vectorized execution for analytics
- **Search Engine**: Full-text search with inverted indexes and WAND optimization
- **Vector Engine**: HNSW indexes with Product Quantization for similarity search
- **Cache Layer**: Multi-level caching with intelligent eviction policies

### 🔥 Performance Optimizations
- **SIMD Acceleration**: AVX2/NEON vectorized operations
- **Parallel Processing**: Rayon thread pool for CPU-intensive tasks
- **Memory Mapping**: Efficient data access with mmap-based storage
- **JIT Compilation**: Query optimization with just-in-time compilation
- **Adaptive Indexing**: Automatic index management and optimization

### 🔌 Compatibility & Integration
- **PostgreSQL Wire Protocol**: Client compatibility for the supported gateway surface
- **Standard SQL Subset**: ANSI-style SQL support with QMvir extensions and documented gaps
- **Multiple APIs**: REST, WebSocket, and native Rust/Python bindings
- **Export Formats**: Parquet, CSV, JSON support
- **Streaming**: Real-time data ingestion and CDC (Change Data Capture)

---

## 📊 Performance

### Benchmark Results

Benchmarks must be interpreted by mode. QMvir has fast local/in-memory scalar read and index paths. Persistent WAL `per_commit_sync` has stricter durability-at-return semantics but durable mutating workloads are currently slower than PostgreSQL in benchmarked cases. Group commit can improve throughput, but it must not be treated as PostgreSQL `synchronous_commit=on` if mutations are acknowledged before fsync.

| Operation | QMvir | PostgreSQL | DuckDB | Improvement |
|-----------|-------|------------|---------|-------------|
| **Point Queries** | 2.3ms | 4.1ms | 1.8ms | **1.8x** vs PG |
| **Range Scans** | 15ms | 28ms | 12ms | **1.9x** vs PG |
| **Aggregations** | 45ms | 89ms | 38ms | **2.0x** vs PG |
| **Full-Text Search** | 8ms | 23ms | N/A | **2.9x** vs PG |
| **Vector Search (1M)** | 12ms | N/A | N/A | **Native** |
| **Hybrid Search** | 18ms | N/A | N/A | **Native** |

*Run your own benchmarks:*
```bash
bash benchmarks/run_benchmark_suite.sh
python3 benchmarks/ci_benchmark_median.py --runs 5
```

---

## 🏗️ Architecture

QMvir uses a **single-engine hybrid architecture** that eliminates data movement between systems:

```
┌─────────────────────────────────────────────────────────────┐
│                    Client Applications                      │
├─────────────────────────────────────────────────────────────┤
│  PostgreSQL Clients  │  REST APIs  │  WebSocket  │  SDKs   │
└─────────────────────┴─────────────┴─────────────┴─────────┘
                              │
                    ┌─────────▼─────────┐
                    │  PostgresGateway  │  ← Wire Protocol v3
                    │  (Tokio TCP)      │
                    └─────────┬─────────┘
                              │
                    ┌─────────▼─────────┐
                    │ NativeSqlEngine  │  ← Query Processing
                    │   SIMD + Cache    │
                    └─────────┬─────────┘
                              │
          ┌───────────────────┼───────────────────┐
          ▼                   ▼                   ▼
┌─────────────┐    ┌───────────────┐    ┌─────────────────┐
│   OLAP      │    │   Search      │    │    Vector       │
│  Engine     │    │   Engine      │    │    Engine       │
│ (Analytics) │    │ (FTS+Inverted)│    │   (HNSW+PQ)     │
└─────────────┘    └───────────────┘    └─────────────────┘
                              │
                    ┌─────────▼─────────┐
                    │   Storage Layer   │
                    │ WAL + Snapshots   │
                    └───────────────────┘
```

---

## 📚 Documentation

| # | Guide | Description |
|---|--------|-------------|
| 1 | [Architecture](docs/QMVIR_ARCHITECTURE.md) | Gateway, engine, storage, enterprise cluster |
| 2 | [Algorithms](docs/QMVIR_ALGORITHMS.md) | Data structures & algorithms (with source map) |
| 3 | [Basic usage](docs/BASIC_USAGE.md) | Install, SQL, search, vector, backup, CLI |
| 4 | [Enterprise HA](docs/ENTERPRISE_HA_GUIDE.md) | Multi-DC deployment, certification, env vars |

**Index:** [docs/README.md](docs/README.md) · **Tiếng Việt HA:** [docs/ENTERPRISE_HA_GUIDE_VI.md](docs/ENTERPRISE_HA_GUIDE_VI.md)

Historical reports and old audits: [docs/_archive/legacy/](docs/_archive/legacy/) (not maintained).

### Links

- **Source:** private repo `virgori/qmvir-db` (collaborators only)
- **Releases:** [virgori/qmvir-releases v6.2.6](https://github.com/virgori/qmvir-releases/releases/tag/v6.2.6)
- **npm:** [qmvir@6.2.6](https://www.npmjs.com/package/qmvir/v/6.2.6)

---

## 🔧 Development

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
│   │   ├── executor/    # Query execution & SIMD
│   │   └── engines/     # Specialized engines
│   └── Cargo.toml
├── repo/                # Distribution package
│   ├── docs/           # Documentation
│   ├── lib/            # Pre-compiled libraries
│   └── README.md       # Repository guide
├── npm/                # Node.js package
├── benchmarks/         # Performance tests
└── tests/             # Integration tests
```

---

## 🤝 Community & Support

- **Issues / discussions:** contact maintainers (source repo is private)
- **Documentation**: [Full documentation site](https://qmvir.readthedocs.io/)

---

## 📄 License

QMvir is distributed under the proprietary license in [LICENSE](LICENSE). See that file for usage and distribution terms.

---

<div align="center">

**[⭐ Star releases](https://github.com/virgori/qmvir-releases) • [📖 Read the docs](https://qmvir.readthedocs.io/) • [🚀 Get started now](#-quick-start)**

Made with ❤️ by the QMvir Team

</div>
