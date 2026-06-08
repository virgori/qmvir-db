/*
 * ProcedureCatalog — Register, look up, and execute stored PL/QM procedures.
 *
 * Port of _py_legacy/qm_core/procedures/catalog.py
 */

use std::collections::HashMap;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::plqm::{PlqmError, PlqmInterpreter, Value};

// ── Parameter types ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParamType {
    Text,
    Int,
    Float,
    Bool,
    Any,
}

impl ParamType {
    pub fn coerce(&self, v: Value) -> Result<Value, PlqmError> {
        match self {
            ParamType::Any => Ok(v),
            ParamType::Text => Ok(Value::Text(v.as_text())),
            ParamType::Int => v.as_i64().map(Value::Int),
            ParamType::Float => v.as_f64().map(Value::Float),
            ParamType::Bool => Ok(Value::Bool(v.truthy())),
        }
    }
}

// ── Procedure parameter ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ProcedureParam {
    pub name: String,
    pub ty: ParamType,
    pub default: Option<Value>,
    pub required: bool,
}

impl ProcedureParam {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ty: ParamType::Any,
            default: None,
            required: true,
        }
    }

    pub fn with_type(mut self, ty: ParamType) -> Self {
        self.ty = ty;
        self
    }
    pub fn with_default(mut self, v: Value) -> Self {
        self.default = Some(v);
        self.required = false;
        self
    }
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }
}

// ── Stored procedure ─────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct StoredProcedure {
    pub name: String,
    pub params: Vec<ProcedureParam>,
    pub body: String,
    pub owner: String,
    pub description: String,
    /// `true` = may modify data; `false` = read-only
    pub volatile: bool,
    pub created_at: f64,
}

impl StoredProcedure {
    pub fn new(name: impl Into<String>, body: impl Into<String>) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        Self {
            name: name.into(),
            params: Vec::new(),
            body: body.into(),
            owner: "system".into(),
            description: String::new(),
            volatile: true,
            created_at: now,
        }
    }

    pub fn with_params(mut self, params: Vec<ProcedureParam>) -> Self {
        self.params = params;
        self
    }
    pub fn with_owner(mut self, o: impl Into<String>) -> Self {
        self.owner = o.into();
        self
    }
    pub fn read_only(mut self) -> Self {
        self.volatile = false;
        self
    }

    /// Validate and coerce args dict against this procedure's parameter spec.
    pub fn validate_args(
        &self,
        args: &HashMap<String, Value>,
    ) -> Result<HashMap<String, Value>, PlqmError> {
        let mut out = HashMap::new();
        for param in &self.params {
            if let Some(v) = args.get(&param.name) {
                out.insert(param.name.clone(), param.ty.coerce(v.clone())?);
            } else if let Some(default) = &param.default {
                out.insert(param.name.clone(), default.clone());
            } else if param.required {
                return Err(PlqmError(format!(
                    "missing required parameter: {}",
                    param.name
                )));
            }
        }
        Ok(out)
    }
}

// ── Execution stats ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone)]
pub struct ProcStats {
    pub exec_count: u64,
    pub total_time_ms: f64,
}

// ── Catalog ──────────────────────────────────────────────────────────

/// Catalog for stored procedures — register, look up, and execute.
pub struct ProcedureCatalog {
    procedures: HashMap<String, StoredProcedure>,
    stats: HashMap<String, ProcStats>,
    interpreter: PlqmInterpreter,
}

impl Default for ProcedureCatalog {
    fn default() -> Self {
        Self {
            procedures: HashMap::new(),
            stats: HashMap::new(),
            interpreter: PlqmInterpreter::new(),
        }
    }
}

impl ProcedureCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn interpreter_mut(&mut self) -> &mut PlqmInterpreter {
        &mut self.interpreter
    }

    /// Register a stored procedure.
    pub fn register(&mut self, proc: StoredProcedure) {
        let key = proc.name.to_ascii_lowercase();
        self.stats.entry(key.clone()).or_default();
        self.procedures.insert(key, proc);
    }

    /// Drop a stored procedure. Returns `true` if it existed.
    pub fn drop(&mut self, name: &str) -> bool {
        let key = name.to_ascii_lowercase();
        if self.procedures.remove(&key).is_some() {
            self.stats.remove(&key);
            true
        } else {
            false
        }
    }

    /// Look up a procedure by name.
    pub fn get(&self, name: &str) -> Option<&StoredProcedure> {
        self.procedures.get(&name.to_ascii_lowercase())
    }

    /// List all registered procedures.
    pub fn list(&self) -> Vec<&StoredProcedure> {
        self.procedures.values().collect()
    }

    /// Execute a registered procedure by name with the given arguments.
    pub fn call(&mut self, name: &str, args: HashMap<String, Value>) -> Result<Value, PlqmError> {
        let key = name.to_ascii_lowercase();
        let proc = self
            .procedures
            .get(&key)
            .ok_or_else(|| PlqmError(format!("procedure not found: {}", name)))?
            .clone();
        let validated = proc.validate_args(&args)?;

        let start = Instant::now();
        let result = self.interpreter.run(&proc.body, validated)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

        let s = self.stats.entry(key).or_default();
        s.exec_count += 1;
        s.total_time_ms += elapsed_ms;

        Ok(result)
    }

    /// Get execution statistics for all procedures.
    pub fn stats(&self) -> &HashMap<String, ProcStats> {
        &self.stats
    }

    /// Number of registered procedures.
    pub fn len(&self) -> usize {
        self.procedures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.procedures.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_catalog() -> ProcedureCatalog {
        let mut cat = ProcedureCatalog::new();
        cat.register(
            StoredProcedure::new("double", "DECLARE r = n * 2; RETURN r;")
                .with_params(vec![ProcedureParam::new("n").with_type(ParamType::Int)]),
        );
        cat
    }

    #[test]
    fn call_procedure() {
        let mut cat = make_catalog();
        let mut args = HashMap::new();
        args.insert("n".to_string(), Value::Int(7));
        let result = cat.call("double", args).unwrap();
        assert_eq!(result, Value::Float(14.0));
    }

    #[test]
    fn missing_required_param_errors() {
        let mut cat = make_catalog();
        let r = cat.call("double", HashMap::new());
        assert!(r.is_err());
    }

    #[test]
    fn drop_procedure() {
        let mut cat = make_catalog();
        assert!(cat.drop("double"));
        assert!(!cat.drop("double"));
        assert_eq!(cat.len(), 0);
    }

    #[test]
    fn procedure_not_found() {
        let mut cat = ProcedureCatalog::new();
        assert!(cat.call("nonexistent", HashMap::new()).is_err());
    }

    #[test]
    fn stats_accumulate() {
        let mut cat = make_catalog();
        let mut args = HashMap::new();
        args.insert("n".to_string(), Value::Int(3));
        cat.call("double", args.clone()).unwrap();
        cat.call("double", args).unwrap();
        let s = cat.stats().get("double").unwrap();
        assert_eq!(s.exec_count, 2);
        assert!(s.total_time_ms >= 0.0);
    }
}
