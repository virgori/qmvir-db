# QMvir Architecture

**Scope:** `qm_engine/` Rust crate  
**Version:** 6.2.8  
**Primary runtime:** `qm` CLI, PostgreSQL wire gateway, and `NativeSqlEngine`  
**Related docs:** [Basic Usage](BASIC_USAGE.md), [Enterprise HA](ENTERPRISE_HA_GUIDE.md), [HTAP](HTAP_GUIDE.md), [Vietnamese architecture](../vi/QMVIR_ARCHITECTURE.md)

## 1. System Role

QMvir is a Rust-first hybrid database engine. The current release is centered on one crate, `qm_engine`, which provides:

- a PostgreSQL wire-protocol gateway for standard clients such as `psql`;
- native SQL execution through `NativeSqlEngine`;
- per-table snapshots and an append-only native SQL WAL;
- backup/restore and PITR-related metadata;
- full-text search and vector search modules;
- HTAP-oriented row/column storage helpers;
- a loopback-only web dashboard/API through `qm_web`;
- opt-in HA/cluster modules with certification commands.

The active production-style path is single-process by default: client traffic enters through `PostgresGateway`, is decoded as PostgreSQL protocol messages, and is executed by `NativeSqlEngine`.

## 2. Runtime Flow

```text
┌───────────────────────────────────────────────────────────────┐
│                         Clients                               │
├──────────────────────┬──────────────────┬─────────────────────┤
│ PostgreSQL clients   │      qm CLI       │ qm_web local API    │
└──────────────────────┴──────────────────┴─────────────────────┘
                              │
                    ┌─────────▼─────────┐
                    │  PostgresGateway  │
                    │  Tokio TCP/Unix   │
                    └─────────┬─────────┘
                              │
                    ┌─────────▼─────────┐
                    │  NativeSqlEngine  │
                    │  SQL execution    │
                    └─────────┬─────────┘
                              │
        ┌─────────────────────┼─────────────────────┐
        ▼                     ▼                     ▼
┌───────────────┐     ┌───────────────┐     ┌───────────────┐
│ HTAP modules  │     │ Search index  │     │ Vector index  │
│ row/column    │     │ FTS/inverted  │     │ HNSW/PQ       │
└───────────────┘     └───────────────┘     └───────────────┘
                              │
                    ┌─────────▼─────────┐
                    │ Persistence       │
                    │ snapshots + WAL   │
                    │ backup/PITR meta  │
                    └───────────────────┘
```

## 3. Main Rust Modules

| Module | Responsibility |
|--------|----------------|
| `gateway/` | PostgreSQL protocol, SCRAM auth, connection state, native SQL dispatch |
| `gateway/native_sql.rs` | Main SQL execution surface, table catalog, WAL/checkpoint, indexes, transactions |
| `parser/` | SQL parser and dispatcher helpers |
| `executor/` | Batch execution, SIMD helpers, joins, hybrid search, JIT expression infrastructure |
| `htap/` | Row/column runtime, durable column segments, MVCC helpers, planner, spill, PITR |
| `storage/` | Secondary binary storage engine components: binary WAL, pages, cache, snapshot, io_uring WAL |
| `index/` | B+Tree, inverted index, HNSW/PQ, mmap vector store, search checkpoint/catalog modules |
| `backup/` | Backup archive format, verification, encryption, snapshot diff, PostgreSQL import compatibility helpers |
| `cluster/` | Opt-in HA building blocks: shard routing, WAL replication, failover, fencing, Raft metadata, 2PC, witness, chaos checks |
| `ipc/` | Shared-memory ring buffer dispatcher for hub/satellite experiments |
| `web/` | Loopback Axum dashboard/API exposed by `qm_web` |
| `cli/` | `qm` command-line surface |

## 4. SQL Gateway

`PostgresGateway` accepts PostgreSQL wire-protocol connections and routes supported SQL to `NativeSqlEngine`. It handles startup, authentication, simple query, and extended-query flows. SCRAM-SHA-256 authentication and catalog-backed users are implemented in the gateway/auth modules.

The gateway-native transaction surface supports `BEGIN`, `COMMIT`, and `ROLLBACK`. The documented isolation level for the native gateway path is READ COMMITTED. Serializable transactions, fixed transaction-level snapshots, and savepoints are not claimed for this path.

## 5. NativeSqlEngine

`NativeSqlEngine` owns the active in-memory catalog and execution path. Its current responsibilities include:

- table DDL and DML;
- query execution for the supported SQL subset;
- per-table snapshots and native SQL WAL replay;
- prepared-plan and prepared-DML fast paths;
- B+Tree, FTS, JSON/trigram, and HNSW index hooks;
- HTAP dirty tracking and column-segment maintenance;
- backup/restore integration.

This is the main release surface. Older Python-era engines have been removed from the active tree.

## 6. Persistence Model

Native SQL persistence uses two main pieces under the data directory:

- per-table bincode snapshots, coordinated through `native_sql.tables.manifest`;
- `native_sql.wal`, an append-only text WAL containing SQL mutation records.

The engine loads snapshots first, then replays WAL delta records. Checkpoints write per-table snapshots, manifests, search/index catalogs, and compatibility markers.

The crate also contains a binary storage engine path in `storage/` with CRC-protected WAL records and page/storage abstractions. That path is a separate storage layer and long-term consolidation target; it should not be described as replacing the native SQL WAL today.

## 7. HTAP, Search, and Vector Paths

HTAP support is implemented through `htap/` plus hooks inside `NativeSqlEngine`. Committed row changes can mark column segments dirty and update PITR metadata. The planner can choose row, column, index, FTS, or vector paths depending on query shape and available metadata.

Full-text search is implemented through inverted-index modules and SQL index hooks. Vector search is implemented through HNSW/PQ modules, catalog support, and exact-scan fallbacks when an ANN index is not available or not appropriate.

## 8. Packaging and Build

The default release build is Rust-only:

```bash
cargo build --manifest-path qm_engine/Cargo.toml --release --no-default-features --bin qm
```

Python bindings are optional behind the `python` feature and are used mainly for bridge/smoke tests. Cross-platform release binaries are built by `.github/workflows/release-binaries.yml` on GitHub-hosted runners, so the local Mac does not need to perform heavy release builds.

`scripts/sync_and_build_release_quizzman.sh` remains as an optional remote build helper for the `quizzman` server.

## 9. HA and Cluster Boundary

Cluster mode is opt-in through `QM_CLUSTER_*` environment variables. The codebase contains modules for sync WAL replication, failover, fencing, metadata Raft, 2PC, STONITH, witness voting, catch-up, readiness scoring, and chaos checks.

These components should be described as HA building blocks with certification gates. Production claims must be tied to actual `qm cluster certify`, soak, and deployment-specific validation results.

## 10. Experimental and R&D Surfaces

The following technologies are intentionally retained because they are important to the product direction, but they should not be presented as generally production-ready without dedicated validation:

- native machine-code JIT beyond the current expression IR/cache and vectorized interpretation;
- adaptive indexing policy and automatic index lifecycle decisions;
- IPC hub/satellite multi-process routing;
- broad CDC/streaming platform behavior;
- multi-DC HA claims outside environments that have passed certification and soak testing.

This document should stay conservative: describe code that exists, identify the active runtime path, and label incomplete or deployment-dependent technology explicitly.
