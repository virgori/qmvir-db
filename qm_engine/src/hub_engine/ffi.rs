#[cfg(feature = "python")]
use pyo3::prelude::*;
#[cfg(feature = "python")]
use tokio::runtime::Runtime;

#[cfg(feature = "python")]
use crate::hub_engine::executor::{execute_parallel_hash_join, parse_rows_from_json_bytes};
#[cfg(feature = "python")]
use crate::hub_engine::types::{ExecutionMode, HubConfig, QueryRequest};
#[cfg(feature = "python")]
use crate::hub_engine::HubEngine;

#[cfg(feature = "python")]
#[pyclass(name = "HubEngine")]
pub struct PyHubEngine {
    inner: HubEngine,
}

#[cfg(feature = "python")]
#[pymethods]
impl PyHubEngine {
    #[new]
    #[pyo3(signature = (data_dir="/tmp/qm_data"))]
    pub fn new(data_dir: &str) -> Self {
        let cfg = HubConfig {
            data_dir: data_dir.to_string(),
            ..Default::default()
        };
        Self {
            inner: HubEngine::new(cfg),
        }
    }

    pub fn start(&self) -> PyResult<()> {
        let rt =
            Runtime::new().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        rt.block_on(self.inner.start())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    #[pyo3(signature = (sql, purpose=None, allow_vector_join=false))]
    pub fn execute_sql(
        &self,
        sql: &str,
        purpose: Option<String>,
        allow_vector_join: bool,
    ) -> PyResult<String> {
        let rt =
            Runtime::new().map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        let req = QueryRequest {
            sql: sql.to_string(),
            mode: ExecutionMode::NativeHotPath,
            purpose,
            allow_vector_join,
        };
        let res = rt
            .block_on(self.inner.execute_request(&req))
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        serde_json::to_string(&res)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }

    #[pyo3(signature = (build_rows_json, probe_rows_json, build_key, probe_key))]
    pub fn execute_hash_join_bytes(
        &self,
        build_rows_json: &[u8],
        probe_rows_json: &[u8],
        build_key: &str,
        probe_key: &str,
    ) -> PyResult<String> {
        let build_rows = parse_rows_from_json_bytes(build_rows_json)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;
        let probe_rows = parse_rows_from_json_bytes(probe_rows_json)
            .map_err(|e| pyo3::exceptions::PyValueError::new_err(e.to_string()))?;

        let res = execute_parallel_hash_join(build_rows, probe_rows, build_key, probe_key)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        serde_json::to_string(&res)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))
    }
}
