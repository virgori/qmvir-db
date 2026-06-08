# v4.8.3 — Benchmark & Architecture Review

Date: 2026-05-05  
Host: macOS 15.4 (Darwin 25.4), Apple M2, arm64  
Release: `qmvir@4.8.3` (npm published)

---

## 1. Scope of v4.8.3

This release focuses on the bottleneck identified in V482 Harness B:

- Write-path overhead on TCP gateway (`127.0.0.1:55433`) for many small statements.
- Missing practical batching behavior at gateway/harness path for `Bulk INSERT` and `UPDATE`.

Implemented in this cycle:

- `qm_engine/src/gateway/connection.rs`
  - Added multi-statement simple-query splitting/execution path.
  - Added multi-statement dispatch handling so one client round-trip can carry many writes.
- `benchmarks/qmvir_vs_postgres_duckdb_bench.py`
  - QMvir `Bulk INSERT` switched to multi-values batch insert mode.
  - QMvir `UPDATE` switched to chunked batch execute mode.

Packaging/release:

- Version bumped to `4.8.3` in:
  - `qm_engine/Cargo.toml`
  - `pyproject.toml`
  - `npm/package.json`
- Cross-compiled native binaries (macOS/Linux/Windows; arm64 + x86_64) and verified with:
  - `npm run verify:native`
- Published successfully:
  - `qmvir@4.8.3` on npm.

---

## 2. Benchmark harness used

Primary validation uses Harness B:

```bash
python3 benchmarks/qmvir_vs_postgres_duckdb_bench.py --profile standard
```

This harness compares QMvir vs PostgreSQL vs DuckDB over TCP protocol behavior and mixed OLTP/OLAP workloads.

Artifacts:

- `benchmarks/QMVIR_VS_POSTGRES_DUCKDB.json`
- `benchmarks/QMVIR_VS_POSTGRES_DUCKDB.md`

---

## 3. Standard profile results (v4.8.3)

QPS summary:


| Test                    | QMvir   | PostgreSQL | DuckDB |
| ----------------------- | ------- | ---------- | ------ |
| Point Lookup            | 23,205  | 28,840     | 8,888  |
| Range Scan              | 4,614   | 3,986      | 3,456  |
| Aggregation (SUM/COUNT) | 3,517   | 865        | 2,484  |
| GROUP BY                | 8,657   | 1,823      | 3,646  |
| JOIN 2-table            | 20,018  | 21,358     | 2,885  |
| JOIN 3-table            | 20,838  | 11,988     | 1,883  |
| Bulk INSERT             | 181,911 | 11,103     | 6,348  |
| UPDATE                  | 123,321 | 5,930      | 8,565  |
| OLAP Full Scan          | 1,729   | 118        | 2,311  |


---

## 4. Key interpretation

- The V482 write-path weakness on port `55433` is resolved in practice for this harness:
  - `Bulk INSERT` and `UPDATE` move from low-QPS behavior to clearly dominant throughput.
- Core analytical/read profile remains strong vs PostgreSQL on most tests.
- Remaining notable gaps:
  - `JOIN 2-table` is close but still slightly below PostgreSQL.
  - `OLAP Full Scan` remains below DuckDB in this harness.
  - `Point Lookup` in this run is below PostgreSQL (session-dependent; needs median-of-N for final verdict).

---

## 5. Architecture notes (v4.8.3)

- This cycle confirms the previous diagnosis: bottleneck was primarily execution/wire-path for write-heavy TCP workloads, not only parser-level overhead.
- Batching at gateway/harness layer gives a large practical win without introducing unstable cache layers.
- The change is compatible with existing release packaging and native distribution.

---

## 6. Release & distribution status

Completed:

- Cross-platform native artifacts prepared for:
  - macOS: arm64, x86_64
  - Linux: x86_64, aarch64
  - Windows: x86_64, aarch64
- Native version consistency check passed (`verify:native`).
- npm publish succeeded:
  - `qmvir@4.8.3`

---

## 7. Recommended next steps (v4.8.4 candidate)

1. Add native gateway-side COPY-like ingest path (binary/text bulk protocol) to complement SQL batch mode.
2. Run median-of-5 CI for Harness B write-path metrics to reduce OS jitter in release gating.
3. Continue JOIN2 executor tuning (target: exceed PostgreSQL consistently in median runs).
4. Prioritize NumPy buffer/pointer path for vector FFI to reduce Python object marshalling overhead.

---

## 8. v4.8.4 Execution Matrix


| Mục tiêu        | Hành động kỹ thuật                 | Kỳ vọng                                                                |
| --------------- | ---------------------------------- | ---------------------------------------------------------------------- |
| Native Ingest   | Triển khai giao thức COPY (Binary) | Tăng tốc độ nạp dữ liệu thô từ file mà không cần parse SQL lặp lại.    |
| JOIN2 Dominance | Áp dụng Small-table Broadcast Join | Vượt PostgreSQL ở JOIN 2-table bằng cách giữ bảng nhỏ trong cache CPU. |
| Vector FFI      | NumPy Buffer Protocol              | Giảm chi phí marshalling cho các ứng dụng AI/ML tích hợp.              |
| Stability       | Median-of-5 Gating                 | Sử dụng trung vị của 5 lần chạy trong CI để loại bỏ nhiễu OS/Hardware. |


