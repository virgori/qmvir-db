use thiserror::Error;

#[derive(Debug, Error)]
pub enum HubError {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("plan error: {0}")]
    Plan(#[from] PlanError),
    #[error("execution error: {0}")]
    Exec(#[from] ExecError),
    #[error("storage error: {0}")]
    Storage(String),
}

#[derive(Debug, Error)]
pub enum PlanError {
    #[error("unsupported statement")]
    UnsupportedStatement,
    #[error("table metadata not found: {0}")]
    MissingTableMeta(String),
    #[error("invalid join shape")]
    InvalidJoin,
}

#[derive(Debug, Error)]
pub enum ExecError {
    #[error("policy violation: {0}")]
    PolicyViolation(String),
    #[error("execution not implemented: {0}")]
    NotImplemented(String),
}
