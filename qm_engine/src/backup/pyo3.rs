//! PyO3 bindings for the Backup & Migration Suite.
//!
//! Exposes `PyBackupEngine`, `PyVerifyEngine`, and `PyRestoreEngine` to Python.

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;
use std::path::{Path, PathBuf};

use crate::backup::backup::{BackupConfig, BackupEngine};
use crate::backup::encrypt;
use crate::backup::predict::PredictEngine;
use crate::backup::restore::{RestoreConfig, RestoreEngine};
use crate::backup::verify::VerifyEngine;
use crate::backup::Compression;

/// Python-facing backup engine.
///
/// Usage:
/// ```python
/// engine = qm_engine.NativeSqlEngine()
/// # ... populate tables ...
/// result = qm_engine.backup(engine, "/tmp/my_backup.qmvb")
/// info = qm_engine.backup_info("/tmp/my_backup.qmvb")
/// verify = qm_engine.backup_verify("/tmp/my_backup.qmvb")
/// ```

/// Create a backup of the engine's tables to a `.qmvb` file.
#[pyfunction]
#[pyo3(signature = (engine, output, compression="lz4", tables=None, include_wal=false))]
pub fn backup(
    engine: &crate::gateway::PyNativeSqlEngine,
    output: &str,
    compression: &str,
    tables: Option<Vec<String>>,
    include_wal: bool,
) -> PyResult<PyObject> {
    let comp = Compression::from_str(compression).ok_or_else(|| {
        PyRuntimeError::new_err(format!(
            "Unknown compression: {compression}. Use 'none', 'lz4', or 'zstd'"
        ))
    })?;

    let config = BackupConfig {
        tables,
        compression: comp,
        include_wal,
        output: PathBuf::from(output),
    };

    let backup_engine = BackupEngine::new(&engine.inner);
    let result = backup_engine
        .run(&config)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("path", &result.path)?;
        dict.set_item("tables_backed_up", result.tables_backed_up)?;
        dict.set_item("total_rows", result.total_rows)?;
        dict.set_item("original_size", result.original_size)?;
        dict.set_item("compressed_size", result.compressed_size)?;
        dict.set_item("duration_ms", result.duration_ms)?;
        dict.set_item("crc32", format!("{:#010x}", result.crc32))?;
        Ok(dict.into())
    })
}

/// Verify the integrity of a `.qmvb` backup file (CRC32 + HMAC check).
#[pyfunction]
pub fn backup_verify(path: &str) -> PyResult<PyObject> {
    let result = VerifyEngine::verify_quick(Path::new(path))
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("ok", result.ok)?;
        dict.set_item("header_valid", result.header_valid)?;
        dict.set_item("footer_valid", result.footer_valid)?;
        dict.set_item("crc_match", result.crc_match)?;
        dict.set_item("hmac_match", result.hmac_match)?;
        dict.set_item("tables", result.tables)?;
        dict.set_item("rows", result.rows)?;
        dict.set_item("errors", result.errors)?;
        Ok(dict.into())
    })
}

/// Get detailed info about a `.qmvb` backup file.
#[pyfunction]
pub fn backup_info(path: &str) -> PyResult<PyObject> {
    let info =
        VerifyEngine::info(Path::new(path)).map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("format_version", info.format_version)?;
        dict.set_item("backup_type", info.backup_type as u8)?;
        dict.set_item("compression", info.compression as u8)?;
        dict.set_item("base_lsn", info.base_lsn)?;
        dict.set_item("end_lsn", info.end_lsn)?;
        dict.set_item("timestamp", info.timestamp)?;
        dict.set_item("table_count", info.table_count)?;
        dict.set_item("total_rows", info.total_rows)?;
        dict.set_item("original_size", info.original_size)?;
        dict.set_item("file_size", info.file_size)?;

        let tables_list = pyo3::types::PyList::empty_bound(py);
        for t in &info.tables {
            let td = pyo3::types::PyDict::new_bound(py);
            td.set_item("name", &t.name)?;
            td.set_item("columns", &t.columns)?;
            td.set_item("types", &t.types)?;
            td.set_item("row_count", t.row_count)?;
            td.set_item("chunk_count", t.chunk_count)?;
            tables_list.append(td)?;
        }
        dict.set_item("tables", tables_list)?;
        Ok(dict.into())
    })
}

/// Restore a `.qmvb` backup into an engine.
///
/// Usage:
/// ```python
/// engine = qm_engine.NativeSqlEngine()
/// result = qm_engine.backup_restore(engine, "/tmp/my_backup.qmvb")
/// ```
#[pyfunction]
#[pyo3(signature = (engine, path, drop_existing=true, tables=None))]
pub fn backup_restore(
    engine: &mut crate::gateway::PyNativeSqlEngine,
    path: &str,
    drop_existing: bool,
    tables: Option<Vec<String>>,
) -> PyResult<PyObject> {
    let config = RestoreConfig {
        source: PathBuf::from(path),
        tables,
        drop_existing,
    };

    let mut restore_engine = RestoreEngine::new(&mut engine.inner);
    let lower = path.to_ascii_lowercase();
    let result = if lower.ends_with(".qmdiff") {
        restore_engine.restore_diff(&config)
    } else if lower.ends_with(".sql") || lower.ends_with(".pgsql") {
        restore_engine.restore_pgdump_with_config(&config)
    } else {
        restore_engine.restore_qmvb(&config)
    }
    .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("tables_restored", result.tables_restored)?;
        dict.set_item("total_rows", result.total_rows)?;
        dict.set_item("duration_ms", result.duration_ms)?;
        Ok(dict.into())
    })
}

/// Estimate backup size and duration without writing to disk.
///
/// Usage:
/// ```python
/// result = qm_engine.backup_predict(engine)
/// print(f"Estimated size: {result['estimated_size_bytes']} bytes")
/// ```
#[pyfunction]
#[pyo3(signature = (engine, compression="lz4"))]
pub fn backup_predict(
    engine: &crate::gateway::PyNativeSqlEngine,
    compression: &str,
) -> PyResult<PyObject> {
    let comp = Compression::from_str(compression).unwrap_or(Compression::Lz4);

    let predict = PredictEngine::new(&engine.inner);
    let result = predict
        .predict_backup(comp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("estimated_size_bytes", result.estimated_size_bytes)?;
        dict.set_item("estimated_duration_ms", result.estimated_duration_ms)?;
        dict.set_item("row_count", result.row_count)?;
        dict.set_item("table_count", result.table_count)?;
        dict.set_item("compression", &result.compression)?;
        Ok(dict.into())
    })
}

/// Encrypt a `.qmvb` backup file using AES-256-GCM + Argon2id.
///
/// Usage:
/// ```python
/// enc_path = qm_engine.backup_encrypt("/tmp/backup.qmvb", "my_password")
/// # => "/tmp/backup.qmvb.enc"
/// ```
#[pyfunction]
pub fn backup_encrypt(path: &str, password: &str) -> PyResult<String> {
    encrypt::encrypt_file(Path::new(path), password)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

/// Decrypt a `.qmvb.enc` backup file using AES-256-GCM + Argon2id.
///
/// Usage:
/// ```python
/// qm_engine.backup_decrypt("/tmp/backup.qmvb.enc", "/tmp/restored.qmvb", "my_password")
/// ```
#[pyfunction]
pub fn backup_decrypt(input: &str, output: &str, password: &str) -> PyResult<()> {
    encrypt::decrypt_file_to(Path::new(input), Path::new(output), password)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
}

/// Create a differential backup (.qmdiff) containing only rows changed since a base backup.
///
/// Usage:
/// ```python
/// result = qm_engine.backup_diff(engine, "/tmp/base.qmvb", "/tmp/changes.qmdiff")
/// print(f"Added: {result['rows_added']}, Modified: {result['rows_modified']}, Deleted: {result['rows_deleted']}")
/// ```
#[pyfunction]
#[pyo3(signature = (engine, base_path, output, compression="lz4"))]
pub fn backup_diff(
    engine: &crate::gateway::PyNativeSqlEngine,
    base_path: &str,
    output: &str,
    compression: &str,
) -> PyResult<PyObject> {
    use crate::backup::snapshot_diff::{DiffConfig, DiffEngine};

    let comp = Compression::from_str(compression).unwrap_or(Compression::Lz4);

    let config = DiffConfig {
        base_backup: PathBuf::from(base_path),
        compression: comp,
        output: PathBuf::from(output),
    };

    let diff_engine = DiffEngine::new(&engine.inner);
    let result = diff_engine
        .create_diff(&config)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?;

    Python::with_gil(|py| {
        let dict = pyo3::types::PyDict::new_bound(py);
        dict.set_item("path", &result.path)?;
        dict.set_item("tables_changed", result.tables_changed)?;
        dict.set_item("rows_added", result.rows_added)?;
        dict.set_item("rows_modified", result.rows_modified)?;
        dict.set_item("rows_deleted", result.rows_deleted)?;
        dict.set_item("diff_size", result.diff_size)?;
        dict.set_item("duration_ms", result.duration_ms)?;
        dict.set_item("base_lsn", result.base_lsn)?;
        dict.set_item("end_lsn", result.end_lsn)?;
        Ok(dict.into())
    })
}
