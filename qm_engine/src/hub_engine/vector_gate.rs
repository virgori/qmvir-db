use crate::hub_engine::errors::ExecError;
use crate::hub_engine::types::{ExecutionMode, QueryRequest};

#[derive(Debug, Clone, Copy)]
pub enum EnforcementPolicy {
    Strict,
    Permissive,
}

pub struct VectorGate {
    policy: EnforcementPolicy,
}

impl VectorGate {
    pub fn new(policy: EnforcementPolicy) -> Self {
        Self { policy }
    }

    pub fn validate_request(&self, req: &QueryRequest) -> Result<(), ExecError> {
        if matches!(self.policy, EnforcementPolicy::Permissive) {
            return Ok(());
        }

        match req.mode {
            ExecutionMode::VectorSuggestionOnly if !req.is_ai_context() => Err(
                ExecError::PolicyViolation("Vector search requires AI suggestion context".into()),
            ),
            _ => Ok(()),
        }
    }
}
