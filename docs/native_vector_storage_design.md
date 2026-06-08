# Native Vector Storage Design

## Current State

`NativeSqlEngine` stores SQL rows in `NativeTable.rows: HashMap<i64, NativeRow>`. Each
`NativeRow` owns a `HashMap<String, Cell>`. Legacy pgvector-compatible tests inserted
vectors into an `embedding TEXT` column as strings like `[0.1,0.2,0.3]`.

The exact SQL vector path builds a vector sidecar cache per table generation:

- cache key: table name plus column name
- payload: row ids, flat `Vec<f32>`, dimension, and precomputed row norms
- invalidation: table generation bump on insert/update/delete/truncate/alter, and explicit
  cache eviction on vacuum/drop

For native rows, this removes both per-query text parsing and cold/rebuild text parsing.
Legacy text rows still fallback to one parse during cache build.

## Implemented Native Storage

The first native-storage step is implemented in the row model:

```rust
Cell::Vector {
    dim: usize,
    data: Vec<f32>,
    norm: f32,
    text: String,
}
```

`parse_value` creates this payload for pgvector literals such as
`'[1,2,3]'::vector`. The `text` field preserves SQL output compatibility:
`Cell::as_text()` returns a pgvector-style string.

If enum size becomes a concern later, use an indirection:

```rust
Cell::Vector(VectorCellId)
```

with a table-local sidecar arena that owns `Arc<[f32]>` payloads and norms.

## Row Cell vs Columnar Sidecar

The current implementation embeds the typed payload in the row cell because that is the
least invasive change and works with existing bincode snapshots. The next step is a
columnar sidecar:

- row remains compatible with existing SQL paths and output formatting
- exact scan reads contiguous flat vectors
- update/delete/vacuum can synchronize one vector sidecar per `(table, column)`
- the current parsed cache shape can become the permanent sidecar shape

Rows can keep either `Cell::Vector` for small/simple deployments or `Cell::VectorRef`
pointing to the sidecar. The sidecar should become the source of truth for vector
distance execution.

## Persistence

Current persistence uses existing `Cell` serde/bincode snapshot support, so
`Cell::Vector` survives checkpoint/reload. A later optimized persistence format should
store vector columns as a typed block per table:

- table name, column name, generation
- dimension
- row id array
- flat little-endian `f32` data
- norm array
- checksum/version header

Snapshot load can reconstruct caches from native row payloads before serving queries if
needed. WAL keeps existing SQL mutation records initially; a later binary WAL record can
avoid serializing vector text.

## Mutation Rules

- INSERT: parse `::vector` literal once into `Cell::Vector`, storing display text for SQL output.
- UPDATE: replace `Cell::Vector` and recompute norm.
- DELETE: remove row id from sidecar or tombstone then compact on vacuum.
- VACUUM: compact sidecar, drop stale/tombstoned row ids, rebuild dense flat data.
- DROP/TRUNCATE: remove sidecar and all cache entries for that table.

## SQL Compatibility

`SELECT embedding` should render pgvector-compatible text from native payload:

```text
[1,2,3]
```

Existing text-vector rows remain supported. The vector cache reports typed vs fallback
rows; native benchmark rows should show `typed_rows=N` and `text_fallback_rows=0`.
A background or explicit migration can later rewrite legacy text rows into `Cell::Vector`.

## Migration Plan

1. Keep parsed cache as the compatibility layer.
2. Add `ColType::Vector(usize)` or vector metadata for text columns used with pgvector operators.
3. Build table-local vector sidecars during insert/update when the target column is vector typed.
4. Make exact scan prefer native sidecar, then parsed cache, then text fallback.
5. Add snapshot/WAL typed vector persistence.
