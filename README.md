# 🚀 QMvir - Hybrid AI-Native Database Engine

<div align="center">

[![npm version](https://badge.fury.io/js/qmvir.svg)](https://badge.fury.io/js/qmvir)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)
[![Rust](https://img.shields.io/badge/rust-1.75+-orange.svg)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-blue.svg)]()

**High-performance hybrid database combining OLTP, OLAP, Full-Text Search, and Vector Search in a single engine**

[Quick Start](#-quick-start) • [Features](#-features) • [Performance](#-performance) • [Documentation](#-documentation)

</div>

---

## 🎯 Overview

QMvir is a **next-generation hybrid database** built in Rust that breaks traditional database boundaries by unifying multiple data paradigms in a single, high-performance engine. Whether you need transactional processing, real-time analytics, semantic search, or vector similarity search - QMvir delivers it all with PostgreSQL-compatible wire protocol.

### 🌟 What Makes QMvir Special?

- **🔥 Hybrid Architecture**: OLTP + OLAP + Search + Vector in one engine
- **⚡ Lightning Fast**: Rust-powered with SIMD optimizations and parallel processing
- **🔌 PostgreSQL Compatible**: Drop-in replacement for existing applications
- **🧠 AI-Native**: Built-in vector search with HNSW and Product Quantization
- **🔍 Full-Text Search**: Advanced inverted index with WAND/BMW optimization
- **📊 Real-time Analytics**: Columnar storage with vectorized execution
- **🛡️ Enterprise Ready**: ACID compliance, backups, security, and monitoring

---

## 🚀 Quick Start

### Installation

#### Option 1: NPM (Recommended)
```bash
npm install -g qmvir
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
- **OLTP Engine**: ACID-compliant transactional processing with MVCC
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
- **PostgreSQL Wire Protocol**: Full compatibility with PostgreSQL clients
- **Standard SQL**: ANSI SQL compliance with extensions
- **Multiple APIs**: REST, WebSocket, and native Rust/Python bindings
- **Export Formats**: Parquet, CSV, JSON support
- **Streaming**: Real-time data ingestion and CDC (Change Data Capture)

---

## 📊 Performance

### Benchmark Results (Median vs PostgreSQL + DuckDB)

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

### Core Documentation
- **[Architecture Guide](repo/docs/QMVIR_ARCHITECTURE.md)** - Deep dive into system design
- **[Algorithm Reference](repo/docs/QMVIR_ALGORITHMS.md)** - Complete algorithm inventory
- **[Repository Guide](repo/README.md)** - Build and usage instructions

### Quick Links
- **GitHub Repository**: [virgori/qmvir-db](https://github.com/virgori/qmvir-db)
- **NPM Package**: [@qmvir](https://www.npmjs.com/package/qmvir)
- **Documentation Site**: [qmvir.readthedocs.io](https://qmvir.readthedocs.io/)

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

- **GitHub Issues**: [Bug reports and feature requests](https://github.com/virgori/qmvir-db/issues)
- **Discussions**: [Community discussions and Q&A](https://github.com/virgori/qmvir-db/discussions)
- **Documentation**: [Full documentation site](https://qmvir.readthedocs.io/)

---

## 📄 License

QMvir is licensed under the [MIT License](LICENSE). See the LICENSE file for details.

---

<div align="center">

**[⭐ Star us on GitHub](https://github.com/virgori/qmvir-db) • [📖 Read the docs](https://qmvir.readthedocs.io/) • [🚀 Get started now](#-quick-start)**

Made with ❤️ by the QMvir Team

</div>

## Documentation

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for full architecture details.
=======
# QMvir / QM Engine: Hybrid Rust Database Core

**QMvir** is a high-performance database engine built in Rust, blending transactional power (OLTP) with analytical scale (OLAP). It features a PostgreSQL-compatible wire protocol and a modular architecture designed for the modern data stack.

---

## ⚡ Key Capabilities

* **Hybrid Engine:** Seamlessly handles row-based operations, complex aggregations, and massive scans.
* **Multi-Modal Search:** Built-in Full-text search (FTS) and Vector search (HNSW) for AI-ready applications.
* **Postgres Compatible:** Connect instantly using `psql` or any standard PG driver.
* **Advanced Storage:** Features Binary WAL, MVCC, Snapshots, and `io_uring` optimization (Linux).
* **V4.8+ Engine Pools:** Dedicated worker pools for Analytics, Vector search, and Compaction to prevent resource contention.

---

## 🏗️ Architecture at a Glance

| Layer | Responsibility |
| --- | --- |
| **Gateway** | PG v3 protocol, SCRAM authentication, CDC hooks. |
| **NativeSqlEngine** | The primary SQL execution hot path. |
| **Executor** | SIMD vectorized kernels, JIT compilation, and hybrid search logic. |
| **Storage** | ACID-compliant WAL, MVCC, and snapshot management. |
| **Engines** | Specialized pools (Rayon) for compute-heavy analytics and vector tasks. |

---

## 🚀 Quick Start

### 1. Build the CLI (Standalone)

```bash
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin qm

```

### 2. Start the Server

```bash
./qm_engine/target/release/qm --data-dir ./data start --foreground --admin-password your-password

```

*QMvir listens on port `55433` by default. Connect via: `psql -p 55433`.*

### 3. Launch HTTP Dashboard (Optional)

```bash
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin qm_web
./qm_engine/target/release/qm_web --port 8080

```

---

## 📦 Ecosystem Support

* **Rust:** Available as the `qm_engine` crate.
* **Node.js/NPM:**
```bash
npm install -g qmvir
qmvir --version

```


* **Python:** Seamless integration via PyO3 (build with default features).

---

## 📊 Benchmarks & Documentation

* **Performance:** Compare against PostgreSQL and DuckDB:

```bash
    bash benchmarks/run_benchmark_suite.sh
    ```
*   **Technical Docs:** 
    *   `docs/QMVIR_ARCHITECTURE.md`: Deep dive into gateway and engine pools.
    *   `docs/QMVIR_ALGORITHMS.md`: Detailed inventory of implemented data structures.
    *   `docs/README.md`: Central documentation index.

---
> **Note:** As of v4.8+, QMvir has migrated its primary SQL and wire logic to the Rust core. Legacy Python modules are archived in `docs/reference/`.

```
>>>>>>> ee323ec991015f1993ec3cde81738d42f3749524
