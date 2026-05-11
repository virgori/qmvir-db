# QMvir Benchmark — VPS Production (Contabo 24GB RAM)

**Date**: 2026-04-15  
**Version**: QMvir v4.3.4  
**Platform**: Linux x86_64 (Contabo VPS, 24 GB RAM)  
**Binary**: `qm-linux-x86_64` (musl, static linked)  
**Profile**: `quick` — 10,000 iterations per benchmark

---

## Results

| # | Benchmark | ops/s | Time |
|---|-----------|------:|-----:|
| 1 | Ring Buffer IPC (1KB publish+consume) | **266.2K** | 15.4 ms |
| 2 | LSN Sequencer (next) | **190.48M** | 525 μs |
| 3 | Cache insert (64B values) | **782.0K** | 12.8 ms |
| 4 | Cache get (mixed hit/miss) | **59.59M** | 839 μs |
| 5 | B+Tree insert | **5.48M** | 1.8 ms |
| 6 | B+Tree point lookup | **6.30M** | 1.6 ms |
| 7 | Roaring Bitmap insert | **8.10M** | 1.2 ms |
| 8 | Roaring Bitmap contains | **416.67M** | 24 μs |
| 9 | JIT batch_filter (eq scan) | **50.76M** | 197 μs |
| 10 | HNSW insert (128d, cosine) | **4.3K** | 1.17 s |
| 11 | HNSW search top-10 (128d) | **30.4K** | 32.9 ms |
| 12 | HyperLogLog add | **250.00M** | 40 μs |
| 13 | Bloom Filter insert | **50.25M** | 199 μs |
| 14 | Bloom Filter lookup | **2.50G** | 4 μs |
| 15 | SQL INSERT (end-to-end) | **1.1K** | 9.39 s |
| 16 | SQL SELECT by PK (end-to-end) | **753.8K** | 6.6 ms |
| 17 | SQL COUNT(\*) aggregation | **574.7K** | 174 μs |

**Total**: 17 benchmarks completed.

---

## Highlights

- **Bloom Filter lookup**: 2.5 billion ops/sec — cache-line-optimized bit array
- **Roaring Bitmap contains**: 416M ops/sec — compressed bitmap with rank/select
- **HyperLogLog add**: 250M ops/sec — streaming cardinality estimation
- **LSN Sequencer**: 190M ops/sec — lock-free atomic monotonic counter
- **SQL SELECT by PK**: 753K ops/sec — full end-to-end (parse → plan → execute → serialize)
- **HNSW search top-10**: 30.4K queries/sec on 128-dimension vectors

## Environment

```
Provider:  Contabo VPS
RAM:       24 GB
OS:        Linux x86_64
Binary:    qm v4.3.4 (musl static, --release, opt-level=3, LTO=thin, panic=abort)
```
