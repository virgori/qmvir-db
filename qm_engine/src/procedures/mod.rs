/*
 * Stored procedure engine — PL/QM interpreter + procedure catalog.
 *
 * Modules:
 *   plqm    — Lexer, Parser, AST, Interpreter, Value
 *   catalog — StoredProcedure, ProcedureCatalog
 */

pub mod catalog;
pub mod plqm;

pub use catalog::{ParamType, ProcedureCatalog, ProcedureParam, StoredProcedure};
pub use plqm::{PlqmError, PlqmInterpreter, Value as PlqmValue};
