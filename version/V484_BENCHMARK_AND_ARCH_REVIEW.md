# v4.8.4 — Benchmark & Architecture Review

Date: 2026-05-05  
Host: macOS 15.4 (Darwin 25.4), Apple M2, arm64  
Release status: `qmvir@4.8.4` published on npm (latest)

---

## 1. Release goals (from V483 execution matrix)

v4.8.4 targeted four concrete items:


| Mục tiêu        | Hành động kỹ thuật                 | Kỳ vọng                                       |
| --------------- | ---------------------------------- | --------------------------------------------- |
| Native Ingest   | Triển khai giao thức COPY (Binary) | Tăng tốc nạp dữ liệu thô, tránh parse SQL lặp |
| JOIN2 Dominance | Small-table Broadcast Join         | Thu hẹp / vượt PostgreSQL ở JOIN 2-table      |
| Vector FFI      | NumPy Buffer Protocol              | Giảm chi phí marshalling cho AI/ML            |
| Stability       | Median-of-5 Gating                 | Loại nhiễu run đơn lẻ trong CI/release        |


---

## 2. What was implemented in code

### 2.1 Native Ingest — COPY BINARY

File: `qm_engine/src/gateway/native_sql.rs`

- Extended `COPY ... FROM` to support `BINARY` format in addition to Parquet.
- Added `copy_from_binary()` native loader:
  - magic header: `QMCOPY1\0`
  - `row_count` + typed row payload by table schema
  - batch insert under a single write-lock scope
  - one cache invalidation per import batch

Result: native ingest path now supports raw binary import without SQL statement loops.

### 2.2 JOIN2 path — broadcast dimension lookup

File: `qm_engine/src/gateway/native_sql.rs`

- Updated fast JOIN2 materialization to use cached dimension map (`get_dim/put_dim`) for account name lookup.
- Reduced per-query row-level lookups in hot JOIN2 path by using small-table broadcast cache behavior.

### 2.3 Vector FFI — NumPy buffer protocol entry points

Files:

- `qm_engine/Cargo.toml`
- `qm_engine/src/executor/mod.rs`

Implemented:

- Added optional `numpy` dependency under Python feature.
- Added NumPy-based APIs:
  - `batch_dot_product_numpy(...)`
  - `batch_search_dot_top_k_numpy(...)`
- These accept `PyReadonlyArray2<f32>` and avoid list-of-lists marshalling overhead.

### 2.4 Stability gate — Median-of-5 in CI

File: `.github/workflows/benchmark-median.yml`

Added explicit median gate step:

- Enforces exactly 5 runs.
- Validates required tests/engines in summary artifact.
- Hard gates write-path:
  - `QMvir Bulk INSERT median >= 2x PostgreSQL`
  - `QMvir UPDATE median >= 2x PostgreSQL`

---

## 3. Benchmark validation (release gate run)

Command used:

```bash
python3 benchmarks/ci_benchmark_median.py --runs 5 --profile quick --output-dir benchmarks
```

Artifact:

- `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_median_summary.json`

Computed gate ratios:

- Bulk INSERT median ratio (QMvir/PostgreSQL): **27.36x**
- UPDATE median ratio (QMvir/PostgreSQL): **11.01x**

Both exceed the 2x hard-gate threshold by a large margin.

---

## 4. Performance snapshot (quick runs during gating)

Observed pattern across 5 runs:

- Write-path metrics (`Bulk INSERT`, `UPDATE`) are consistently dominant for QMvir.
- `Range Scan`, `GROUP BY`, `JOIN 3-table` remain strong vs PostgreSQL.
- `JOIN 2-table` still trails PostgreSQL in these runs.
- `OLAP Full Scan` remains below DuckDB in this harness.

Interpretation:

- v4.8.4 successfully addresses the write-path bottleneck diagnosed in V482/V483.
- Remaining performance frontier is mostly JOIN2 and OLAP scan competitiveness.

---

## 5. Cross-compile and packaging status

Cross-compiled and staged native binaries for:

- macOS: `arm64`, `x86_64`
- Linux: `x86_64`, `aarch64`
- Windows: `x86_64`, `aarch64`

Verification:

- `npm run verify:native` passed for all packaged targets with embedded version `4.8.4`.

Publish:

- `npm publish --access public` succeeded.
- Final package: `**qmvir@4.8.4`**

---

## 6. Comparison vs v4.8.3

Delta summary:

- v4.8.3 proved write-path optimization concept in harness behavior.
- v4.8.4 productized it with:
  - formal COPY binary ingest path,
  - NumPy FFI interface additions,
  - median-of-5 gating enforcement in workflow.

Net effect:

- The release process is now tied to statistically stable write-path criteria, not single-run outcomes.

---

## 7. Known remaining gaps

1. JOIN 2-table still below PostgreSQL median in current quick harness.
2. OLAP Full Scan still below DuckDB in this profile.
3. NumPy APIs currently optimize marshalling path; further zero-copy/vectorized internal kernels can still improve throughput.

---

## 8. Next-step recommendations (v4.8.5 candidate)

1. Add native COPY stream mode (chunked network ingest), not only file-based binary format.
2. Continue JOIN2 executor tuning (broadcast + predicate/vectorized probe refinement) targeting PostgreSQL overtake in median runs.
3. Add dedicated OLAP morsel scheduling and vectorized filter/projection refinements for DuckDB parity+.
4. Expand CI gates with JOIN2 and OLAP minimum ratio thresholds after stability window confirms low false positives.

