# Benchmark Scripts

Included scripts:

- `full_benchmark.py`: full benchmark suite
- `backup_predict_chaos.py`: backup/predict/diff/restore chaos harness
- `../scripts/release_benchmark_native_sql.py`: NativeSqlEngine and Python gateway release gate benchmark

Run examples:

```bash
python3 full_benchmark.py
QM_BIN=../qmvir python3 backup_predict_chaos.py
python3 ../scripts/release_benchmark_native_sql.py --quick --build-mode dev --feature-flags default
python3 ../scripts/release_benchmark_native_sql.py --iterations 1000 --output ../docs/native_sql_benchmark_last.json --build-mode release --feature-flags default
```

The quick release benchmark is intended for CI smoke gating. The full local run
records p50, p95, p99 latency, throughput, peak RSS, OS/CPU/Python/Rust
metadata, and a baseline comparison against
`docs/native_sql_benchmark_baseline.json` when that baseline has approved
results.
