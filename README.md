# QMvir

<div align="center">

[![Release Gate](https://github.com/virgori/qmvir-db/actions/workflows/release-gate.yml/badge.svg)](https://github.com/virgori/qmvir-db/actions/workflows/release-gate.yml)
[![License: Custom Source Available](https://img.shields.io/badge/license-custom%20source--available-red.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org/)
[![Platforms](https://img.shields.io/badge/platform-Linux%20%7C%20macOS%20%7C%20Windows-blue.svg)](.github/workflows/release-binaries.yml)

**A Rust-first hybrid database engine for SQL, analytics, full-text search, and vector search.**

[Quick start](#quick-start) · [Capabilities](#capabilities) · [Architecture](#architecture) · [Documentation](#documentation)

</div>

## Overview

QMvir combines a PostgreSQL wire-protocol gateway with native SQL execution,
WAL/checkpoint persistence, analytical row/column paths, full-text search, and
vector indexing. The active implementation lives in the `qm_engine` Rust crate.

The default runtime is centered on `PostgresGateway` and `NativeSqlEngine`.
Cluster, learned-optimization, JIT, IPC, and streaming components also exist in
the repository, but some are opt-in or experimental and are not assumed to be
part of every execution path.

## Quick start

### Requirements

- A current stable Rust toolchain
- Clang 14 or newer for native builds
- `psql` or another PostgreSQL-compatible client for wire-protocol access

### Build

```bash
cargo build \
  --manifest-path qm_engine/Cargo.toml \
  --release \
  --no-default-features \
  --bin qm
```

### Start the server

```bash
./qm_engine/target/release/qm \
  --data-dir ./data \
  start \
  --admin-password 'replace-this-password'
```

The gateway listens on `127.0.0.1:55433` by default.

```bash
psql -h 127.0.0.1 -p 55433 -U admin -d qm
```

For foreground mode, configuration, and additional commands, see the
[basic usage guide](docs/en/BASIC_USAGE.md) or run:

```bash
./qm_engine/target/release/qm guide
```

## Capabilities

### Active runtime surface

- Native SQL DDL, DML, and query execution for the supported SQL subset
- PostgreSQL wire protocol for supported client operations
- Per-table snapshots, append-only SQL WAL, checkpoints, and recovery
- Gateway-native `BEGIN`, `COMMIT`, and `ROLLBACK` with documented transaction limits
- HTAP-oriented row/column storage paths and vectorized execution helpers
- Full-text indexes and search
- HNSW-based vector indexes with product-quantization support
- Backup, restore, verification, encryption, and PITR-related metadata
- `qm` command-line administration and the optional `qm_web` local dashboard
- Optional HA and cluster components, including replication, failover, 2PC, fencing, and certification tooling
- Optional PyO3 bindings behind the `python` feature

### Performance building blocks

The codebase includes SIMD helpers, Rayon parallelism, memory-mapped storage,
adaptive indexing, statistics, learned models, expression JIT infrastructure,
and Linux `io_uring` WAL support. Availability in the source tree does not imply
that every component is wired into the default SQL hot path.

Treat native-code JIT, learned planning, hub/satellite IPC, CDC/streaming, and
multi-DC deployment as experimental or deployment-specific until they have been
validated for the intended workload.

## Architecture

```text
PostgreSQL clients     qm CLI            qm_web
         \               |                 /
          +--------------+----------------+
                         |
                 PostgresGateway
                         |
                 NativeSqlEngine
                         |
       +-----------------+-----------------+
       |                 |                 |
   HTAP paths      Full-text search    Vector search
       |                 |                 |
       +-----------------+-----------------+
                         |
                 WAL + snapshots
```

The crate is organized by responsibility:

- `gateway/`: PostgreSQL protocol, sessions, authentication, and native SQL
- `parser/`, `optimizer/`, `executor/`: parsing, planning, and execution components
- `htap/`, `mvcc/`: row/column paths, visibility, isolation, and PITR helpers
- `index/`, `search/`: scalar, full-text, and vector indexes
- `storage/`, `backup/`: WAL, snapshots, cache, backup, and restore
- `cluster/`: opt-in routing, replication, consensus, failover, and fencing
- `hub_engine/`, `ipc/`, `learned/`, `statistics/`: advanced and experimental components
- `cli/`, `web/`: operational interfaces

## Development

The repository disables Cargo's automatic target discovery, so binaries,
examples, benchmarks, and integration tests are declared explicitly in
[`qm_engine/Cargo.toml`](qm_engine/Cargo.toml).

```bash
# Static dependency/type check
cargo check --manifest-path qm_engine/Cargo.toml

# Rust test suite without optional Python bindings
cargo test --manifest-path qm_engine/Cargo.toml --no-default-features

# Criterion benchmarks
cargo bench --manifest-path qm_engine/Cargo.toml

# Focused native SQL benchmark
cargo run \
  --manifest-path qm_engine/Cargo.toml \
  --release \
  --no-default-features \
  --bin native_sql_core_bench
```

Release binaries for Linux, macOS, and Windows are defined in
[`release-binaries.yml`](.github/workflows/release-binaries.yml). Pull requests
and pushes to `main` are checked by
[`release-gate.yml`](.github/workflows/release-gate.yml).

## Benchmarking

Benchmark results depend on durability mode, hardware, dataset, warm-up, and
client behavior. In particular, relaxed or group-commit modes must not be
presented as equivalent to PostgreSQL `synchronous_commit=on` unless their
acknowledgement and durability guarantees match.

Any published comparison must identify the QMvir version and commit, build
profile, configuration, hardware, workload, iteration count, and methodology.
See the [PostgreSQL comparison guide](docs/internal/POSTGRES_COMPARISON.md).

## Repository layout

```text
qmvir-db/
├── qm_engine/       # Rust engine crate, binaries, tests, and benchmarks
├── docs/            # User, architecture, operations, and maintainer docs
├── scripts/         # Release, profiling, comparison, and HA helpers
├── tests/           # Optional Python/PyO3 integration tests
├── include/         # C-facing API header
└── .github/         # CI, release, and repository hygiene workflows
```

## Documentation

- [Documentation index](docs/README.md)
- [Architecture](docs/en/QMVIR_ARCHITECTURE.md)
- [Basic usage](docs/en/BASIC_USAGE.md)
- [HTAP guide](docs/en/HTAP_GUIDE.md)
- [Enterprise HA guide](docs/en/ENTERPRISE_HA_GUIDE.md)
- [Vietnamese documentation](docs/vi/)

## Project status

QMvir is source-available software under active development. Validate
correctness, durability, compatibility, and performance against your own
workload before deployment. Experimental modules and optional cluster paths
require additional target-environment certification.

## License and contact

QMvir is distributed under the
[qmvir-db Commercial Use and Restricted Redistribution License v1.0](LICENSE),
a custom source-available license that is not OSI-approved.

The license permits personal, research, and commercial use, including paid
applications and services that do not expose QMvir as a general-purpose
Database Service. Private modifications and private forks are permitted and do
not have to be published. Separate written permission or a separate license is
required for redistribution, public derivative repositories, embedding copies
in distributed products, or offering QMvir as a Database Service.

The `LICENSE` file is authoritative. This summary is provided for convenience
and does not replace the license terms.

- Source: [github.com/virgori/qmvir-db](https://github.com/virgori/qmvir-db)
- Licensing and permission requests: [virgorilabs@gmail.com](mailto:virgorilabs@gmail.com)
- Technical support: [support@virgori.com](mailto:support@virgori.com)

<div align="center">

Developed by **Virgori**.

</div>
