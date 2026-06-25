/*
 * QM Engine - High-Performance Database Core in Rust
 *
 * Components:
 * 1. Gateway - PostgreSQL Wire Protocol (async tokio)
 * 2. Parser - SQL Parser & Query Dispatcher
 * 3. Executor - Vectorized Execution Engine (SIMD)
 * 4. Storage - ACID WAL & Storage Engine
 */

#[cfg(feature = "python")]
use pyo3::prelude::*;

pub mod backup;
pub mod cli;
pub mod cluster;
pub mod executor;
pub mod gateway;
pub mod hub_engine;
pub mod index;
pub mod ipc;
pub mod learned;
pub mod metrics;
pub mod mvcc;
pub mod optimizer;
pub mod parser;
pub mod procedures;
pub mod search;
pub mod statistics;
pub mod storage;
pub mod types;
pub mod web;

// Re-exports
pub use cluster::{
    cluster_router_env_enabled, ClusterRuntime, ConsistentHashRing, MetaCluster, NodeClient,
    QmRouter, ReplicaSet, ReplicationConfig, RoutePlan, ShardGroupCatalog, ShardGroupKind,
    ShardManager, TransportServer, TwoPhaseCoordinator, TxnPhase, WalEntry, WorkloadClass,
};
pub use executor::{ColumnBatch, ExecutionResult, VectorExecutor};
pub use gateway::{ConnectionConfig, NativeSqlEngine, PostgresGateway};
pub use hub_engine::HubEngine;
pub use index::{BPlusTree, IndexKey, IndexManager};
pub use index::{HnswIndex, HnswPqIndex, InvertedIndex, RoaringBitmap};
pub use learned::{CachePredictor, FusionWeightTuner, IntentClassifier, SelectivityModel};
pub use metrics::MetricsRegistry;
pub use optimizer::{AdaptiveOptimizer, LogicalPlan};
pub use parser::{ParsedQuery, QueryType, SqlParser};
pub use statistics::{BloomFilter, CostModel, CountMinSketch, HyperLogLog, TDigest};
pub use storage::{StorageEngine, Transaction, WalWriter};

/// Python module initialization
#[cfg(feature = "python")]
#[pymodule]
fn qm_engine(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Gateway
    m.add_class::<gateway::PyPostgresGateway>()?;
    m.add_class::<gateway::PyNativeSqlEngine>()?;
    m.add_class::<gateway::native_sql::PyNativeBuffer>()?;

    // Parser
    m.add_class::<parser::PySqlParser>()?;

    // Executor
    m.add_class::<executor::PyVectorExecutor>()?;

    // Storage
    m.add_class::<storage::PyStorageEngine>()?;
    m.add_class::<storage::PyTransaction>()?;

    // Cluster routing + HA primitives (transport, 2PC, WAL streaming)
    m.add_class::<cluster::shard::PyShardRing>()?;
    m.add_class::<cluster::shard::PyShardManager>()?;
    m.add_class::<cluster::transport::PyNodeTransport>()?;
    m.add_class::<cluster::two_phase_commit::PyDistributedCoordinator>()?;
    m.add_class::<storage::wal_streaming::PyWalSender>()?;
    m.add_class::<storage::wal_streaming::PyWalReceiver>()?;

    // Index
    m.add_class::<index::PyIndexManager>()?;

    // Hub Engine (Rust-native coordinator)
    m.add_class::<hub_engine::ffi::PyHubEngine>()?;

    // IPC Ring Buffer
    m.add_class::<ipc::ring_buffer::PyRingBuffer>()?;

    // IPC Native Dispatcher
    m.add_class::<ipc::dispatcher::PyNativeDispatcher>()?;

    // W-TinyLFU Cache
    m.add_class::<storage::PyWTinyLfuCache>()?;

    // io_uring WAL Writer
    m.add_class::<storage::PyUringWalWriter>()?;

    // JIT Compiler
    m.add_class::<executor::PyJitCompiler>()?;

    // Backup / restore functions
    m.add_function(wrap_pyfunction!(backup::pyo3::backup, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_verify, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_info, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_restore, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_predict, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_encrypt, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_decrypt, m)?)?;
    m.add_function(wrap_pyfunction!(backup::pyo3::backup_diff, m)?)?;

    Ok(())
}
