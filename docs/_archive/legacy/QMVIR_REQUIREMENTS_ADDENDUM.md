# QMvir Requirements Addendum (ANSI SQL, CLI Ops, Chaos)

## 1. QM-SQL Language Requirements

QMvir must treat ANSI SQL as the default query language surface.

Allowed ANSI SQL baseline:
- `SELECT`
- `INSERT`
- `UPDATE`
- `DELETE`
- `JOIN`
- `GROUP BY`
- `ORDER BY`
- `LIMIT`

QM-specific extensions (allowed as explicit extensions only):
- `LIKEV` / `VEC`: vector search operations
- `MREF`: media blob reference operations
- `CPOINT`: manual checkpoint command

Implementation notes:
- Parser/linter should reject non-standard aliases that overlap with ANSI semantics.
- Docs and CLI help must mark extension commands as `QM-Extensions` explicitly.
- Benchmark SQL in `benchmarks/qmvir_vs_postgres_bench.py` must remain ANSI-compatible.

## 2. CLI Operational Checklist (`qmvir`)

- [ ] Daemon control (`qmvir start/stop/status`) reflects real Rust Gateway PID and current backend mode.
- [ ] SQL shell (`qmvir sql`) supports tab completion and command history parity with `psql` workflows.
- [ ] Monitoring (`qmvir dash`) shows per-satellite latency and queue depth.
- [ ] Log access (`qmvir logs`) supports split view/filtering by layer:
  - `gateway-rust`
  - `satellite-python`
  - `satellite-vir`

Suggested commands:
- `qmvir logs --layer gateway-rust`
- `qmvir logs --layer satellite-python --tail 200`
- `qmvir logs --layer all --follow`

## 3. Benchmark Safety and Chaos Engineering

### 3.1 Mandatory Chaos Scenarios

- Hard Kill:
  - send `SIGKILL` to core under 100% write load (target 50k TPS)
  - system recovery target: `< 2s` after restart
- Network Partition:
  - isolate Hub <-> Satellite link
  - Hub must buffer commands in ring buffer and replay on reconnect
- Memory Pressure:
  - consume up to ~95% host RAM
  - verify storage `LRU` page eviction avoids OOM and keeps service available

### 3.2 Durability Gates

- WAL vs OS cache:
  - benchmark with/without `O_SYNC`
  - any acknowledged commit must survive restart according to configured durability mode
- Checksum verification:
  - each data page (4KB/8KB) includes CRC32
  - `qmvir check` must detect corrupted pages
- Auto-backup/checkpoint:
  - trigger checkpoint when WAL reaches threshold (example: 64MB)
  - goal: reduce recovery time objective (RTO)

## 4. Disaster Dashboard (CLI) Draft

Recommended new command:
- `qmvir dash-chaos`

Minimum panels:
- Chaos run status:
  - current scenario, stage, elapsed time, pass/fail
- Recovery SLA:
  - restart latency, command replay lag, RTO/RPO counters
- Buffer and WAL health:
  - ring occupancy, backpressure level, WAL write/fsync latency
- Memory pressure:
  - RSS, allocator pressure, eviction/sec, reclaimed bytes
- Integrity:
  - page checksum errors, repaired pages, last `qmvir check` result

Output modes:
- TUI (default)
- JSON stream (`--json`) for CI ingestion

## 5. Current Implementation Delta (2026-03-09)

- Default QMvir gateway port separated from PostgreSQL default to avoid conflicts (`55433`).
- Rust Native SQL join path now includes:
  - cost-based strategy selection using row counts and selectivity
  - index join for selective predicates
  - hash join with SoA + SIMD filter path (AVX2/NEON) and parallel probe (Rayon)
- Benchmark report updated at:
  - `benchmarks/QMVIR_VS_POSTGRES.md`
