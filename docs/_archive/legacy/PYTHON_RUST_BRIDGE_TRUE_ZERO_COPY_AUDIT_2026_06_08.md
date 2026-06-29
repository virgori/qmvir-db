# QMvir Python/Rust Bridge True Zero-Copy Audit

Audit date: 2026-06-08

## Scope

This audit covers the NativeSqlEngine result path behind:

- `NativeSqlEngine.execute(...)`
- `NativeSqlEngine.execute_columnar(...)`
- native SQL simple table scans
- vector query fallback behavior
- BM25/hybrid bridge feasibility

## Existing Result Pipeline

The legacy public result type is:

```rust
pub struct QueryResult {
    pub columns: Vec<(String, i32, i16)>,
    pub rows: Vec<Vec<Option<Vec<u8>>>>,
    pub command_tag: String,
}
```

This is the row/cell materialization boundary. Once a query becomes
`Vec<Vec<Option<Vec<u8>>>>`, typed integer/float/text ownership has already been
flattened into per-cell byte vectors.

## Where Values Become Rows

The primary conversion points are in `qm_engine/src/gateway/native_sql.rs`:

- `handle_select_all`: uses `CachedColumns`, then converts each cell to
  `to_string().into_bytes()` or `as_bytes().to_vec()`.
- `handle_select_order_limit`: collects sorted row ids and materializes each
  selected cell to bytes.
- vector `ORDER BY embedding <-> ...` paths build scored rows and then output
  `Vec<Vec<Option<Vec<u8>>>>`.
- joins, aggregates, subqueries, CTEs, set operations, and returning clauses
  all construct `QueryResult` rows directly.

## Existing Typed Data

The engine already has a typed column cache:

```rust
struct CachedColumns {
    ids: Vec<i64>,
    int_cols: AHashMap<String, Vec<i64>>,
    float_cols: AHashMap<String, Vec<f64>>,
    text_cols: AHashMap<String, Vec<String>>,
}
```

That cache is sorted by id and is suitable for a direct columnar result path for
simple selected table scans. It is not currently an Arrow buffer and does not
track null bitmaps in a way that can be borrowed as-is.

## Copy Timeline

Legacy row API:

1. Table rows are stored as `Cell` values.
2. `CachedColumns` may be built from table rows for supported scans.
3. Legacy `QueryResult` converts each selected value into `Vec<u8>`.
4. PyO3 `execute(...)` converts `Vec<u8>` into Python `str`.

Previous reduced-copy API:

1. Query still produces `QueryResult`.
2. PyO3 `execute_columnar(...)` packs row bytes into Python-owned `bytes`.
3. It avoids Python row/cell strings but does not avoid Rust row/cell bytes.

New zero-copy-capable path:

1. Supported SQL bypasses `QueryResult`.
2. The engine creates `NativeColumnarBatch` columns from typed cached data.
3. Python receives `memoryview` objects over Rust-owned `Arc` buffers.
4. Python does not copy exposed result buffers.

## Supported Direct Path

Supported:

- `SELECT cols FROM table`
- `SELECT cols FROM table ORDER BY id [LIMIT n]`
- integer columns
- float columns
- text columns as offsets/data buffers
- mixed int/float/text
- empty result sets

Unsupported and explicitly fallback:

- non-SELECT statements
- WHERE
- joins
- aggregates/grouping
- set operations
- CTEs/subqueries
- expression projections
- vector distance ORDER BY
- ORDER BY columns other than id
- ORDER BY id DESC/OFFSET

## Ownership

`NativeColumnarBatch` uses stable shared Rust ownership:

- `Arc<[i64]>`
- `Arc<[f64]>`
- `Arc<[u8]>`

PyO3 `NativeBuffer` owns those `Arc` values and implements the Python buffer
protocol. Python `memoryview` objects keep the buffer owner alive through the
buffer protocol, so views remain valid even if the result dict is deleted.

## Claim Status

True zero-copy is proven only for the Python bridge exposure of supported
direct columnar buffers. It does not mean the storage engine itself is Arrow
native or that every SQL execution path avoids internal copies.

Unsupported SQL remains `REDUCED_COPY_FALLBACK`.
