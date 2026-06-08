# v4.8.2 — Benchmark & Architecture Review

Date: 2026-05-05  
Host: macOS 15.4 (Darwin 25.4), Apple M2, arm64  
Python: 3.13.x (Frameworks install) — `qmvir` **4.8.2** wheel at  
`/Library/Frameworks/Python.framework/Versions/3.13/lib/python3.13/site-packages`  
Peers: PostgreSQL (local socket `/tmp`, port 5432), DuckDB **1.5.2** (`pip`), psycopg2 **2.9.11**.

This report follows the structure of [V480_BENCHMARK_AND_ARCH_REVIEW.md](V480_BENCHMARK_AND_ARCH_REVIEW.md): same harnesses, updated numbers for **v4.8.2**, and comparison points against the **v4.8.1 benchmark snapshot on disk** (`benchmarks/FULL_BENCHMARK_v481.json`, 2026-05-04) and the **published v4.8.0-era comparison table** (§10 in the V480 doc: “Đánh giá đối đầu DuckDB/Postgres”, 100 K rows, `standard` profile).

---

## 1. Work summarized (v4.8.2 release focus)

Functional themes shipped for this line (see repo history / release notes):

- **Read-path hot loop:** less `Cell` cloning in `native_sql` (borrowed `&Cell`, `NULL_CELL`, `as_text_borrowed` / `to_text_bytes`), fewer allocations on Range Scan + join probe paths.
- **Concurrency:** `NativeSqlEngine.tables` migrated to `parking_lot::RwLock` (faster read-side critical sections than `std::sync::RwLock`).
- **Vector isolation:** `PyVectorExecutor` dispatches through the dedicated `VectorEngine` runtime (resource isolation; see §5 for microbench implications).
- **Versioning / packaging:** `env!("CARGO_PKG_VERSION")` at CLI/gateway; cross-build hygiene (e.g. non-Windows `mimalloc`); `qmvir@4.8.2` on npm.

This document does **not** re-run `cargo test`; the V480/V481 reports remain the reference for last full library test counts unless you refresh them in CI.

---

## 2. Harness A — `benchmarks/full_benchmark.py` (Layer 1–3)

Commands:

```bash
python3 benchmarks/full_benchmark.py --profile standard --json benchmarks/FULL_BENCHMARK_v482_on.json
export QM_ANALYTICS_ENGINE=0 QM_VECTOR_ENGINE=0 QM_COMPACTOR_ENGINE=0
python3 benchmarks/full_benchmark.py --profile standard --json benchmarks/FULL_BENCHMARK_v482_engines_off.json
unset QM_ANALYTICS_ENGINE QM_VECTOR_ENGINE QM_COMPACTOR_ENGINE
```

**Profile `standard`:** 100 000 account rows, same seed and ops counts as previous reports.

QMvir uses `**PostgresGateway.start_native()`** (in-process server on **127.0.0.1:15434**). Startup banner confirmed `**QMvir v4.8.2`**.

Artifacts:

- `benchmarks/FULL_BENCHMARK_v482_on.json` — engines default **ON**
- `benchmarks/FULL_BENCHMARK_v482_engines_off.json` — engines **OFF** (inline / v4.7-style paths)
- `benchmarks/FULL_BENCHMARK.json` — copy of the **ON** run (latest canonical)

---

## 3. Layer 2 — QMvir vs PostgreSQL vs DuckDB vs SQLite (`standard`)

Source: `FULL_BENCHMARK_v482_batch_api.json` (2026-05-05, same run as §6). Rates are **ops/s**.


| Query (Layer 2) | QMvir | PostgreSQL | DuckDB | SQLite |
| --------------- | ----- | ---------- | ------ | ------ |
| Point Lookup    | 32.0K | 23.6K      | 8.74K  | 158.1K |
| Range Scan      | 1.89K | 696        | 2.49K  | 1.76K  |
| Aggregation     | 8.31K | 120        | 743    | 163    |
| GROUP BY        | 3.92K | 550        | 1.92K  | 469    |
| JOIN            | 25.1K | 12.5K      | 2.22K  | 72.7K  |
| INSERT          | 15.7K | 10.1K      | 4.45K  | 161.7K |
| UPDATE          | 29.8K | 8.16K      | 7.10K  | 157.7K |
| DELETE          | 32.0K | 8.69K      | 8.0K   | 232.8K |


**Quick read:** QMvir remains ahead of PostgreSQL and DuckDB on **Point Lookup** and most OLTP/aggregate mixes in this session; **DuckDB leads Range Scan**; **SQLite** dominates single-connection embedded reads/writes (different deployment model — not apples-to-apples with TCP gateway).

---

## 4. Comparison to the v4.8.1 snapshot file (same harness, different day)

> **Note:** §3 uses the latest `**FULL_BENCHMARK_v482_batch_api`** run. The **v4.8.2** column in the table below still refers to an **earlier single run** (`_v482_on` / median session), not re-synchronised to every new benchmark file — use for **trend vs v4.8.1 snapshot** only.

The repository keeps a frozen JSON from **2026-05-04** (`FULL_BENCHMARK_v481.json`, ops/s rounded):


| Query        | v4.8.1 snapshot (QMvir) | v4.8.2 run (QMvir) | Note                                                                   |
| ------------ | ----------------------- | ------------------ | ---------------------------------------------------------------------- |
| Point Lookup | 32.4K                   | 29.0K              | Session / thermal variance typical for single-threaded gateway benches |
| Range Scan   | 2.34K                   | 1.25K              | Large swing — treat as **environment noise** unless median-of-N        |
| Aggregation  | 9.96K                   | 5.83K              | Same caveat                                                            |
| GROUP BY     | 5.56K                   | 3.77K              | Same caveat                                                            |
| JOIN         | 33.6K                   | 23.9K              | Same caveat                                                            |
| INSERT       | 38.5K                   | 25.2K              | Same caveat                                                            |
| UPDATE       | 39.2K                   | 16.4K              | Same caveat                                                            |
| DELETE       | 40.4K                   | 20.9K              | Same caveat                                                            |


**Interpretation:** The **directional** story (QMvir vs Postgres vs DuckDB) is stable, but **absolute QMvir OLTP numbers move ±10–40%** between sessions on a laptop without pinning CPU isolation. **Do not** infer a code regression from this two-sample comparison alone.

### 4.1 Median of 3 runs (V482 only) — follow-up 2026-05-05

Procedure: three **sequential** `full_benchmark.py --profile standard` runs (engines default ON), same machine as §2–§4. Artifacts: `benchmarks/FULL_BENCHMARK_v482_median_run{1,2,3}.json`; aggregated `**benchmarks/FULL_BENCHMARK_v482_median_summary.json`**.

**QMvir Layer 2 — ops/s (median of 3 vs single snapshot `FULL_BENCHMARK_v481.json`)**


| Query        | Run 1 | Run 2 | Run 3 | **Median** | v481 snapshot | Median / v481 |
| ------------ | ----- | ----- | ----- | ---------- | ------------- | ------------- |
| Point Lookup | 26.0K | 5.7K  | 26.2K | **26.0K**  | 32.4K         | **0.80**      |
| Range Scan   | 2.12K | 1.78K | 1.93K | **1.93K**  | 2.34K         | **0.82**      |
| Aggregation  | 9.54K | 11.9K | 6.44K | **9.54K**  | 9.96K         | **0.96**      |
| GROUP BY     | 2.02K | 3.87K | 3.66K | **3.66K**  | 5.56K         | **0.66**      |
| JOIN         | 27.2K | 28.6K | 18.3K | **27.2K**  | 33.6K         | **0.81**      |
| INSERT       | 22.3K | 17.6K | 23.4K | **22.3K**  | 38.5K         | **0.58**      |
| UPDATE       | 21.2K | 22.9K | 27.8K | **22.9K**  | 39.2K         | **0.59**      |
| DELETE       | 28.9K | 30.1K | 31.2K | **30.1K**  | 40.4K         | **0.74**      |


**Within-V482 spread** (max − min, as % of median) was very large on **Point Lookup (~78%)** and material on **Aggregation (~58%)**, **GROUP BY (~51%)** — run 2 behaved like a different machine for several tests (background load / scheduling).

**Kết luận (statistical, không phải “release verdict”):**

1. **Median-of-3 vẫn thấp hơn một mẫu v481 lưu trên disk** trên hầu hết chỉ số (khoảng **0.58–0.96×**). Điều đó **không** chứng minh “V482 chậm hơn V481 trong code”, vì file v481 chỉ là **một** phiên đo (không median, không khoảng tin cậy), và **cùng harness trên laptop có outlier mạnh** (ví dụ Point Lookup run 2).
2. Để kết luận phiên bản một cách **phòng thí nghiệm**: checkout **v4.8.1** (hoặc tag), chạy **cùng median-3** trong **cùng cửa sổ idle**, hoặc máy bench cố định / CI; so **median vs median**, không phải **median vs một ngày cũ**.
3. **Aggregation** (median **0.96×** v481) là chỗ gần nhất giữa hai nguồn số — gợi ý sự chênh phần lớn các chỉ số khác đến từ **nhiễu đo + một snapshot lịch sử**, chứ chưa có bằng chứng rằng tối ưu v4.8.2 “phá” OLTP trong harness này.

---

## 5. Engine split A/B — same binary, same profile (v4.8.2)

`FULL_BENCHMARK_v482_engines_off.json` vs `FULL_BENCHMARK_v482_on.json`.


| Query        | Engines OFF | Engines ON | Δ (ON vs OFF) |
| ------------ | ----------- | ---------- | ------------- |
| Point Lookup | 18.0K       | 29.0K      | +61%          |
| Range Scan   | 2.21K       | 1.25K      | −42%          |
| Aggregation  | 8.52K       | 5.83K      | −32%          |
| GROUP BY     | 5.36K       | 3.77K      | −30%          |
| JOIN         | 31.9K       | 23.9K      | −25%          |
| INSERT       | 32.5K       | 25.2K      | −23%          |
| UPDATE       | 32.9K       | 16.4K      | −50%          |
| DELETE       | 34.5K       | 20.9K      | −40%          |


**Note:** These deltas mix (a) real scheduler effects, (b) which subsystems touch analytics/vector/compactor paths, and (c) run-to-run noise. For **policy**, the cost gate + lazy pools in v4.8.1 still apply; v4.8.2 does not change the analytics gate semantics—this A/B is informational.

---

## 6. Layer 3 — Vector microbench (`full_benchmark.py`, standard)

Workload: **10 000 × 256** stored vectors, **200** query vectors.  
Source: `FULL_BENCHMARK_v482_batch_api.json` (2026-05-05), `QMvir v4.8.2`, after **batch FFI API** (`executor/mod.rs`: `batch_dot_product_queries`, `batch_l2_distance_queries`, `batch_search_dot_top_k`).


| Test                     | QMvir-SIMD (per-query loop) | QMvir-Rayon | **QMvir-Batch (single FFI)** | NumPy  |
| ------------------------ | --------------------------- | ----------- | ---------------------------- | ------ |
| Dot Product              | 71                          | 80          | **1 596**                    | 11 630 |
| L2 Distance              | 62                          | —           | **1 316**                    | 540    |
| Top-10 search (dot sort) | 72                          | —           | **1 427**                    | —      |


**Per-query loop:** mỗi query là một lần gọi PyO3 — QPS thấp; SIMD vẫn nặng do **chi phí FFI lặp**.

**Batch-FFI:** một lần gọi xử lý đủ **200** query cùng một `vectors` list; cột `ops/s` dùng `ops = 200` (cùng định nghĩa “query throughput” với vòng lặp). **~22×** so với per-query SIMD trên Dot Product trong run này.

NumPy Dot vẫn ~**7×** nhanh hơn Batch-FFI trong cùng harness (toán học thuần trong C, không deserialize `list[list[float]]` như PyO3). Để SDK sản phẩm, có thể bổ sung **NumPy buffer / buffer protocol** sau.

---

## 7. Harness B — `benchmarks/qmvir_vs_postgres_duckdb_bench.py` (DuckDB-focused)

**Purpose:** Separate dataset layout (50 K accounts, 5 K products, 200 K orders), **parameterized SQL**, psql-style workloads, and (for QMvir) a **standalone TCP server** on **127.0.0.1:55433** — not the same code path as `PostgresGateway.start_native()` in Harness A.

**How we ran QMvir here:** Release CLI `qm` built with `**cargo build --release --no-default-features --bin qm`** (Python extension not linked into the binary):

```bash
qm --data-dir benchmarks/v482_qm_server_data start --foreground --port 55433 --host 127.0.0.1
python3 benchmarks/qmvir_vs_postgres_duckdb_bench.py --profile standard --pg-db benchdb --output-dir benchmarks
qm --data-dir benchmarks/v482_qm_server_data stop
```

Artifacts: `benchmarks/QMVIR_VS_POSTGRES_DUCKDB.md` and `.json` (timestamp **2026-05-05**).

**QPS summary (standard):**


| Test                    | QMvir  | PostgreSQL | DuckDB |
| ----------------------- | ------ | ---------- | ------ |
| Point Lookup            | 24 138 | 18 208     | 6 957  |
| Range Scan              | 4 005  | 3 481      | 3 440  |
| Aggregation (SUM/COUNT) | 3 741  | 730        | 2 405  |
| GROUP BY                | 6 728  | 1 372      | 2 677  |
| JOIN 2-table            | 13 777 | 13 976     | 2 742  |
| JOIN 3-table            | 24 373 | 12 677     | 1 867  |
| Bulk INSERT             | 329    | 12 075     | 4 267  |
| UPDATE                  | 329    | 2 003      | 7 556  |
| OLAP Full Scan          | 1 248  | 113        | 2 233  |


**Interpretation:** **PostgreSQL wins** narrow **JOIN 2-table** and especially **Bulk INSERT/UPDATE** here; **DuckDB wins UPDATE** (and OLAP scan vs QMvir in this harness). These results are **not** comparable 1:1 with §3 — different client, different data volume, and TCP server vs embedded gateway.

**Write-path note (Harness B / port 55433):**

- QMvir đạt QPS thấp ở Bulk INSERT/UPDATE trong harness này vì đang xử lý nhiều statement nhỏ qua TCP wire protocol theo kiểu tuần tự.
- Đây chủ yếu là chi phí gateway/protocol path (parse/bind/execute/flush cho từng statement), chưa có pipeline/batching đủ mạnh ở lớp gateway.
- PostgreSQL đang có lợi thế rõ ở batch write protocol path, nên khoảng cách ~329 vs ~12K QPS có thể xuất hiện ở profile này.

---

## 8. Historical reference — V480 document §10 (2026-04-24)

The markdown table in V480 (100 K rows, `standard`) recorded, for example, **QMvir Point ~32.4 K**, **Range ~2.3 K**, **DuckDB Range ~2.6 K**. The **2026-05-05** run in §3 is **qualitatively aligned** (QMvir > Postgres on most OLTP reads; Range Scan competitive with DuckDB varies by session). Use **median-of-3** runs if you need publication-grade absolutes.

---

## 9. Architecture / product notes (v4.8.2)


| Topic                                    | Status / comment                                                                                                                                                        |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cell decode + `parking_lot` RwLock       | Landed — reduces allocator traffic and lock overhead on hot read paths (see code in `native_sql.rs`).                                                                   |
| VectorEngine integration from Python FFI | Mặc định SIMD **inline** trên luồng Python; `QM_VECTOR_PYTHON_ISOLATION=1` bật lại dispatch qua engine. **Batch API** (`batch_*_queries`) để khấu trừ FFI (§6).         |
| Harness clarity                          | `**full_benchmark.py`** = in-process gateway + SQLite + comprehensive stack. `**qmvir_vs_postgres_duckdb_bench.py`** = multi-table **TCP** QMvir vs Postgres vs DuckDB. |


---

## 10. Recommended next steps (optional)

1. **Done for this host:** three sequential runs + `FULL_BENCHMARK_v482_median_summary.json` (§4.1). For a **fair vs v4.8.1 code** verdict, repeat median-of-3 on a **checkout of v4.8.1** under the same idle conditions (or use a pinned CI runner).
2. **Done:** **Batch FFI** trên `VectorExecutor` (§6) — một lần gọi cho toàn bộ query batch.
3. Priority cho v4.8.3: hỗ trợ **NumPy buffer/pointer path** (buffer protocol) từ Python vào Rust để tránh deserialize `list[list[float]]` trong vector FFI.
4. For **Harness B** (`55433`), nghiên cứu **COPY/batch-mode** ở gateway để thu hẹp khoảng cách Bulk Write với PostgreSQL.
5. Chuẩn hóa release scripts cho Linux/macOS (arm64 + x86_64) để benchmark/release matrix nhất quán đa nền tảng.

---

## 11. Files touched by this benchmark session


| File                                                    | Role                                                                               |
| ------------------------------------------------------- | ---------------------------------------------------------------------------------- |
| `benchmarks/FULL_BENCHMARK_v482_on.json`                | v4.8.2, engines ON (single run)                                                    |
| `benchmarks/FULL_BENCHMARK_v482_engines_off.json`       | v4.8.2, engines OFF                                                                |
| `benchmarks/FULL_BENCHMARK.json`                        | Trùng `FULL_BENCHMARK_v482_batch_api.json` — run sau batch vector API (2026-05-05) |
| `benchmarks/FULL_BENCHMARK_v482_batch_api.json`         | `standard`, có thêm vector **batch-FFI** trong Layer 3                             |
| `benchmarks/FULL_BENCHMARK_v482_median_run{1,2,3}.json` | Raw median study (2026-05-05)                                                      |
| `benchmarks/FULL_BENCHMARK_v482_median_summary.json`    | Median Layer-2 QMvir + ratios vs v481 snapshot                                     |
| `benchmarks/QMVIR_VS_POSTGRES_DUCKDB.json` / `.md`      | Harness B, standard profile                                                        |


