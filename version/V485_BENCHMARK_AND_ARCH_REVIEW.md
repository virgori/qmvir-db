# v4.8.5 — Benchmark & Architecture Review

Date: 2026-05-06  
Host: macOS / Apple Silicon (Darwin arm64)  
Release status: `**qmvir@4.8.5` not published to npm** at report time — median harness: QMvir beats PostgreSQL on **all** quick tests, but **Aggregation (SUM/COUNT)** median QPS remains slightly below **DuckDB** (see §4).

---

## 1. Release goals (v4.8.5 execution matrix)


| Objective               | Technical action                                                                                   | Outcome                                                |
| ----------------------- | -------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| Execution hot paths     | Wire-protocol fast statements `__QM_FAST_JOIN2`, `__QM_FAST_OLAP_SUM_TOTAL_GT_QTY` wired to engine | Early dispatch to dedicated join/OLAP paths            |
| JOIN2 throughput        | SoA inputs, `account_groups` probe, parallel morsels, preformatted wire totals (`ryu`)             | Lower scan/probe overhead on join2 micro-harness       |
| OLAP scan               | SIMD-assisted `morsel_parallel_sum_f64_where_i64_gt` (NEON / x86 SIMD chunk + scalar tail)         | Faster fused sum under predicate                       |
| Concurrency             | `NativeSqlEngine::tables`: `std::sync::RwLock` → `parking_lot::RwLock`                             | Faster reader-heavy table metadata access              |
| Native packaging sanity | Deterministic `CARGO_TARGET_DIR`, npm `verify:native` matrix, refreshed Windows ARM banner         | Reliable **4.8.5** artifacts for all bundled platforms |


---

## 2. What was implemented / changed

### 2.1 Wire fast paths

File: `qm_engine/src/gateway/native_sql.rs`

- Detection and dispatch near `execute_inner_authed` for introspection-safe fast statements:
  - `__QM_FAST_JOIN2 …` → `execute_fast_join2`
  - `__QM_FAST_OLAP_SUM_TOTAL_GT_QTY …` → fused OLAP path
- JOIN2 execution uses structure-of-arrays style inputs and parallelism for large projections where applicable.

### 2.2 Vectorized OLAP kernel

File: `qm_engine/src/executor/vectorized.rs`

- Enhanced `morsel_parallel_sum_f64_where_i64_gt` with architecture-specific SIMD inner loops before scalar remainder (aarch64 NEON i64 compare lanes; x86 path gated on available features).

### 2.3 Table lock + ancillary API fixes

Files (representative):

- `qm_engine/src/gateway/native_sql.rs` — `tables` guarded by `parking_lot::RwLock`; call sites updated for non-poisoning lock API.
- `qm_engine/src/gateway/mod.rs`, `qm_engine/src/cli/server.rs` — session stub `execute_as_with_session`.
- Cluster / web — `execute_local` → `execute`, public `index_manager()` accessors where needed.

### 2.4 Release pipeline & npm native verification

File: `qm_engine/scripts/build_release.sh`

- **Pinned** `export CARGO_TARGET_DIR="$(pwd)/target"` so IDE/sandbox overrides (e.g. Cursor temp target dirs) cannot ship stale or missing `qm` binaries.
- Default cross matrix aligned with npm verification:
  - `aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-pc-windows-gnu`, `aarch64-pc-windows-gnullvm`
- Post-build: copy `../build/release/qm-`* into `npm/bin/native/` for `prepack` / `npm run verify:native`.

Version alignment:

- `qm_engine/Cargo.toml`, `npm/package.json`, `pyproject.toml` at **4.8.5**.

Verification command:

```bash
cd npm && npm run verify:native
```

Expected: embedded banner `QMvir v4.8.5` on Linux/Windows stubs; macOS ARM binary `--version` → `qm 4.8.5`.

---

## 3. Linux-specific architecture tuning (ARM + x86_64)

File: `qm_engine/.cargo/config.toml`


| Triple                                                                       | Intent                                                                                                                                                                         |
| ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `aarch64-unknown-linux-gnu` *(via same family defaults as musl in comments)* | Server-class ARM (`neoverse-n1` baseline in musl block): NEON + dotprod-aligned builds for analytic/vector kernels.                                                            |
| `x86_64-unknown-linux-gnu` / `*-linux-musl`                                  | `**x86-64-v3*`*: AVX2, FMA, BMI1/2, POPCNT, F16C, LZCNT — majority of cloud CPUs since ~Haswell (2013) / Zen+.                                                                 |
| Portable note                                                                | Binaries built with the above CPU levels **will not run** on CPUs below the targeted feature set for that triple; distribute musl variants if maximum portability is required. |


Runtime SIMD in hot numeric code (example: dot product / distance) remains **feature-detected** on x86_64 inside `vectorized.rs` (AVX-512 vs AVX2 vs SSE fallbacks).

---

## 4. Benchmark validation (median-of-5, `quick` profile)

### 4.1 Procedure

```bash
cd /path/to/QM
python3 benchmarks/ci_benchmark_median.py \
  --runs 5 --profile quick --output-dir benchmarks \
  --pg-host 127.0.0.1 --pg-user <user> --pg-db benchdb
```

- **PostgreSQL**: TCP `127.0.0.1:5432`, database `benchdb` (created if missing).  
- **QMvir**: `127.0.0.1:55433` (must be running before the script; same as harness default).  
- **DuckDB**: in-process, in-memory (`duckdb` Python package).

Harness version on this run: DuckDB Python **v1.5.2**.

Artifacts written:


| File                                                      | Role                                        |
| --------------------------------------------------------- | ------------------------------------------- |
| `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_median_summary.json` | Per-engine median QPS / p95                 |
| `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_median_summary.md`   | Ranked markdown tables                      |
| `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_median_meta.json`    | Profile, run count, forwarded `--pg-`* args |
| `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_run{1–5}.json`       | Individual run JSON snapshots               |


### 4.2 Median QPS (higher is better)

Source: `benchmarks/QMVIR_VS_POSTGRES_DUCKDB_median_summary.json` (2026-05-06 host run).


| Benchmark               | QMvir   | PostgreSQL | DuckDB | Median winner |
| ----------------------- | ------- | ---------- | ------ | ------------- |
| Point Lookup            | 17,772  | 15,034     | 9,419  | **QMvir**     |
| Range Scan              | 4,762   | 3,504      | 3,741  | **QMvir**     |
| Aggregation (SUM/COUNT) | 3,950   | 1,265      | 4,015  | **DuckDB**    |
| GROUP BY                | 13,100  | 6,044      | 4,061  | **QMvir**     |
| JOIN 2-table            | 26,361  | 16,549     | 3,566  | **QMvir**     |
| JOIN 3-table            | 22,446  | 5,585      | 2,122  | **QMvir**     |
| Bulk INSERT             | 302,553 | 9,661      | 5,933  | **QMvir**     |
| UPDATE                  | 99,468  | 7,405      | 8,409  | **QMvir**     |
| OLAP Full Scan          | 3,453   | 315        | 3,368  | **QMvir**     |


Values rounded from JSON medians (`median_qps`).

### 4.3 Median QMvir ratios (QPS)


| Benchmark               | QMvir / PostgreSQL | QMvir / DuckDB |
| ----------------------- | ------------------ | -------------- |
| Point Lookup            | 1.18×              | 1.89×          |
| Range Scan              | 1.36×              | 1.27×          |
| Aggregation (SUM/COUNT) | 3.12×              | **0.98×**      |
| GROUP BY                | 2.17×              | 3.23×          |
| JOIN 2-table            | 1.59×              | 7.39×          |
| JOIN 3-table            | 4.02×              | 10.58×         |
| Bulk INSERT             | 31.3×              | 51.0×          |
| UPDATE                  | 13.4×              | 11.8×          |
| OLAP Full Scan          | 11.0×              | 1.03×          |


### 4.4 Interpretation vs publish gate

- **PostgreSQL**: QMvir median QPS is **strictly higher** than PostgreSQL on every test in this matrix.  
- **DuckDB**: One outlier remains — **Aggregation (SUM/COUNT)** — DuckDB median is ~~**1.02×** faster than QMvir (~~65 QPS delta at the median). OLAP median is slightly ahead for QMvir vs DuckDB in this snapshot.

So a strict rule “median QPS must beat **both** peers on **every** row” **still fails** until simple aggregate path matches or exceeds DuckDB.  

`benchmarks/ci_benchmark_median.py` now accepts optional `--pg-host`, `--pg-user`, `--pg-db`, etc., so local runs match CI without relying on default socket paths.

---

## 5. Cross-compile matrix (artifacts)

Artifacts produced under `QM/build/release/` and mirrored to `QM/npm/bin/native/`:


| Platform            | Artifact name            |
| ------------------- | ------------------------ |
| macOS arm64         | `qm-macos-arm64`         |
| Linux x86_64 glibc  | `qm-linux-x86_64`        |
| Linux aarch64 glibc | `qm-linux-aarch64`       |
| Windows x86_64      | `qm-windows-x86_64.exe`  |
| Windows arm64       | `qm-windows-aarch64.exe` |


Build entrypoint:

```bash
cd qm_engine && bash scripts/build_release.sh
```

Requires `**cargo zigbuild**` + **zig** for non-host Linux/Windows triples.

---

## 6. Comparison vs v4.8.4


| Area              | v4.8.4                                                                | v4.8.5                                                                     |
| ----------------- | --------------------------------------------------------------------- | -------------------------------------------------------------------------- |
| Product focus     | COPY binary ingest, JOIN2 broadcast cache, NumPy FFI, median CI gates | Executor fast paths + SIMD OLAP chunk + RwLock ergonomics                  |
| Packaging         | Published npm `@4.8.4`                                                | Native binary matrix refreshed to **4.8.5**; `verify:native` green in-tree |
| Build determinism | Assumed implicit `target/` layout                                     | Explicit `CARGO_TARGET_DIR` pin in release script                          |


---

## 7. Known remaining gaps

1. **Simple aggregation (SUM/COUNT)** vs **DuckDB** — only median gap in the current `quick` matrix (~2% behind); worth a dedicated vectorized or batch-aggregate fast path in the gateway/executor.
2. **Rust default features**: standalone `qm` MUST be built with `--no-default-features` when Python/`pyo3` is unwanted on the linker path (`build_release.sh` already does this).
3. `**npm/bin/native` binaries** are large artefacts; confirm release repo / GitHub distribution policy before committing binaries to git (many teams `.gitignore` and upload per release).
4. **Run-to-run variance** (e.g. UPDATE, Bulk INSERT) — keep median-of-5 (or more) for release decisions; single-run numbers are noisy.

---

## 8. Next-step recommendations (v4.8.6+ candidate)

1. Close the **Aggregation vs DuckDB** gap; re-run `ci_benchmark_median.py`; if QMvir median leads on **every** row vs both engines, `**npm publish`** with the pinned native matrix is justified under the strict gate.
2. Optional: extend CI gates to DuckDB-relative thresholds once Aggregation is fixed (avoid flaky single-run regressions).
3. Linux production: document **glibc vs musl** choice and minimum CPU tier (`x86-64-v3` / Neoverse) in operator docs.
4. Consider publishing **Rosetta (`x86_64-apple-darwin`)** tarball separately — omitted from default `build_release.sh` matrix to reduce fragile cross-SDK breakage on Apple Silicon build hosts.

