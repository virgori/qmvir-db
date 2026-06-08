/*
 * Search module — full-text search platform utilities.
 *
 * Modules:
 *   synonym  — SynonymEngine (bidirectional + one-way expansion)
 */

pub mod synonym;

pub use synonym::SynonymEngine;
