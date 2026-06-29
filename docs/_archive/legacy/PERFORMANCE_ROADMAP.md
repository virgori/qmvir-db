# QMvir Performance Optimization Roadmap

## Executive Summary

This document outlines the technical roadmap for:
1. **Multi-Core CPU / GPU Parallelization** — Maximize throughput on modern hardware
2. **Python-to-C Migration** — Reduce interpreter overhead for hot paths

Based on benchmark analysis (10K vectors, 128 dims, 1K queries):

| Metric | Current | Target | Gap |
|--------|---------|--------|-----|
| HNSW Build | 59 ops/s | 10,000+ ops/s | ~170x |
| HNSW Search | 156 QPS | 50,000+ QPS | ~320x |
| Multi-Thread Scaling | 0.21x@8T | 6-7x@8T | Lock contention |
| Recall@10 | 0.6434 | 0.95+ | Parameter tuning |

## Delta Update (2026-03-13, v0.4.0)

Canonical migration checklist: `docs/PY_TO_RS_MIGRATION_TRACKER.md`

### Delivered (v0.4.0)

1. **Parquet Integration**: `COPY table FROM 'file.parquet'` via Arrow 53 / Parquet 53 (zstd, snap, lz4).
2. **ColumnExtractor enum**: Zero-copy Arrow→Cell conversion with type widening.
3. **Chunk-based Pipeline**: `CHUNK_SIZE=1024` for all major SELECT paths (full scan, BETWEEN, SUM, GROUP BY).
4. **Parallel GROUP BY**: Local hash tables per 1024-row chunk → merge pattern.
5. **Batch INSERT lock optimization**: Pre-build all rows, single write lock.
6. **Benchmark result (v0.4.0)**: 8/9 wins vs PG 16 + DuckDB 1.5.

| Benchmark | QMvir QPS | vs PG | vs DuckDB |
|-----------|--------:|------:|------:|
| Point Lookup | 17,514 | 2.6x | 5.6x |
| Aggregation | 6,031 | 4.4x | 2.6x |
| GROUP BY | 13,227 | 2.1x | 3.5x |
| JOIN 2-table | 15,607 | 1.2x | 5.9x |
| JOIN 3-table | 14,874 | 2.7x | 9.2x |
| Bulk INSERT | 22,661 | 3.2x | 11.6x |
| UPDATE | 14,153 | 1.8x | 4.7x |
| OLAP Scan | 9,529 | 47x | 3.6x |
| Range Scan | 2,728 | 0.84x | 1.15x |

7. **Remaining optimization target**: Range Scan (only benchmark PG wins, 2,728 vs 3,261).

## Delta Update (2026-03-09)

Canonical migration checklist: `docs/PY_TO_RS_MIGRATION_TRACKER.md`

### Delivered

1. Rust hot path now includes native on-disk table scan for hub-engine hash join execution.
2. Parallel hash join kernel (Rayon) is active in Rust executor path.
3. Planner now emits explicit build/probe join keys and handles table alias mapping for ON equality joins.
4. Benchmark launcher supports 1M-row Rust stress case via `RUST_CASES=1000000`.
5. Python benchmark path includes `SHADOW_MODE=1` hook to compare PostgreSQL vs Rust-join outputs.

### Measured Result (1M rows, Rust microbench)

Recent run:
- Sequential join: `824.781 ms`
- Rayon parallel join: `525.764 ms`
- Effective speedup: `1.57x`

Observed range across runs: `~1.5x-1.9x`.

### Latest Verified Run (2026-03-09, Post B+Tree Index Optimization)

1. NativeSqlEngine B+Tree index fast-path eliminates O(n) full table scan for JOIN/SUM queries.
2. PostgreSQL vs QM macrobench (`standard`, 20K accounts, 15K orders, 800 ops):

| Metric | PostgreSQL | QMvir | Speedup (QM/PG) |
|--------|-----------|-------|-----------------|
| JOIN QPS | 5,633 | 5,713 | **1.01x** |
| JOIN p95 (ms) | 0.302 | 0.210 | **0.70x** (QM lower) |
| SUM QPS | 3,370 | 2,890 | 0.86x |
| SUM p95 (ms) | 0.965 | 0.425 | **0.44x** (QM lower) |
| Stress (8 clients, 20s) | 15,676 QPS | 18,879 QPS | **1.20x** |

3. Shadow compare: `SHADOW_MODE=1` — QM returns 59 rows, PG returns 102 rows (difference due to QM table suffix mismatch in shadow query). Float precision mismatch (REAL 4B vs FLOAT8 8B) accounts for remaining mismatches.
4. Improvement from previous: JOIN QPS **147 → 5,713** (39x), SUM QPS **373 → 2,890** (7.7x).

### Root Causes Fixed

1. **Missing B+Tree indexes**: Benchmark created indexes on PostgreSQL but not QM. Fixed: `setup_data()` now creates QM indexes.
2. **handle_select_join() O(n) full scan**: Added B+Tree index fast-path using `tree.search(&IndexKey::Integer(account_id))`.
3. **handle_select_sum() wrong column**: Was hardcoded to "balance", fixed to parse actual column from SQL.
4. **handle_select_between() wrong filter**: Was filtering by row ID, fixed to filter by specified column.
5. **Shadow compare wrong engine**: Used HubEngine (disk) instead of NativeSqlEngine (in-memory). Fixed to use wire protocol.

---

# Part 1: Multi-Core CPU / GPU Parallelization Roadmap

## 1.1 Current State Analysis

### Architecture Overview
```
┌─────────────────────────────────────────────────────────────┐
│                    QMvir Architecture                       │
├─────────────────────────────────────────────────────────────┤
│  Control Plane (Python, fallback only)                      │
│  ├── SQL Parser (Lark LALR) → Rust NativeSqlEngine          │
│  ├── Query Planner → Rust hub_engine planner                │
│  └── Hub Dispatcher                                         │
├─────────────────────────────────────────────────────────────┤
│  Data Plane (Rust NativeSqlEngine + Python fallback)        │
│  ├── NativeSqlEngine (in-memory, B+Tree, SIMD, Rayon)      │
│  ├── GeneralSatellite (row storage, Python fallback)       │
│  ├── VectorSatellite (HNSW + XOR-Delta)                    │
│  └── ProcedureSatellite (PL/QM)                            │
├─────────────────────────────────────────────────────────────┤
│  IPC Layer                                                  │
│  ├── Ring Buffers (thread-safe queues)                     │
│  └── Media Slab Allocator                                  │
└─────────────────────────────────────────────────────────────┘
```

### Identified Bottlenecks

| Component | Issue | Impact |
|-----------|-------|--------|
| `HNSWIndex.search()` | Global RWLock on every search | Negative MT scaling |
| `VectorSatellite._vectors` | Python dict (GIL-bound) | Single-threaded access |
| `_vec_search()` | Sequential neighbor traversal | No SIMD utilization |
| Distance computation | Pure Python loops | 10-100x slower than C |
| Ring buffer IPC | msgpack ser/deser per op | High latency overhead |

### Benchmark Evidence

```
Multi-Threading Scaling (Current):
| Threads | QPS   | Speedup |
|---------|-------|---------|
| 1       | 194   | 1.00x   |
| 2       | 104   | 0.53x   | ← WORSE than 1T
| 4       | 47    | 0.24x   | ← SEVERE contention
| 8       | 41    | 0.21x   | ← GIL + RWLock
```

## 1.2 Phase 1: Lock-Free HNSW (Week 1-2)

### Goal
Eliminate global RWLock contention for read-heavy workloads.

### Implementation

#### 1.2.1 Read-Copy-Update (RCU) Pattern
```python
# Before: Global RWLock blocks all readers during write
class HNSWIndex:
    def search(self, query):
        with self._rwlock.read():  # ← Contention point
            ...

# After: Lock-free reads with epoch-based reclamation
class HNSWIndexRCU:
    def __init__(self):
        self._graph = AtomicRef(HNSWGraph())
        self._epoch = AtomicCounter()
    
    def search(self, query):
        # Lock-free read: snapshot current graph
        epoch = self._epoch.load()
        graph = self._graph.load()
        return self._search_impl(graph, query)
    
    def add(self, id, vector):
        # Copy-on-write: create new graph version
        old = self._graph.load()
        new = old.copy()
        new.insert(id, vector)
        self._graph.store(new)  # Atomic pointer swap
        self._epoch.increment()
```

#### 1.2.2 Per-Layer Sharding
```python
# Partition graph by layer for reduced contention
class ShardedHNSW:
    def __init__(self, num_shards=8):
        self._shards = [HNSWLayer() for _ in range(num_shards)]
    
    def search(self, query):
        # Parallel search across shards
        with ThreadPoolExecutor(num_shards) as ex:
            results = ex.map(lambda s: s.search(query), self._shards)
        return merge_results(results)
```

### Expected Outcome
- Read throughput: **10-20x improvement** with concurrent queries
- Write throughput: Unchanged (still serialized)

## 1.3 Phase 2: SIMD-Accelerated Distance (Week 2-3)

### Goal
Replace pure-Python distance computation with SIMD-optimized kernels.

### Implementation Options

#### Option A: NumPy + OpenBLAS (Quick Win)
```python
# Current: Pure Python loops
def _cosine_dist(a, b):
    dot = float(np.dot(a, b))  # Already uses BLAS
    na = float(np.linalg.norm(a))
    nb = float(np.linalg.norm(b))
    return 1.0 - dot / (na * nb)

# Optimized: Batch distance computation
def _batch_cosine_dist(query, vectors):
    # Single BLAS call for all distances
    dots = vectors @ query  # (N,) matrix-vector
    norms = np.linalg.norm(vectors, axis=1)
    nq = np.linalg.norm(query)
    return 1.0 - dots / (norms * nq + 1e-10)
```

#### Option B: Numba JIT (Medium Effort)
```python
from numba import njit, prange

@njit(parallel=True, fastmath=True)
def batch_l2_distances(query, vectors):
    n = vectors.shape[0]
    dists = np.empty(n, dtype=np.float32)
    for i in prange(n):  # Parallel loop
        d = 0.0
        for j in range(query.shape[0]):
            diff = query[j] - vectors[i, j]
            d += diff * diff
        dists[i] = d
    return dists
```

#### Option C: FAISS Integration (Best Performance)
```python
import faiss

class FAISSBackend:
    def __init__(self, dim):
        # Use IVF-Flat for large datasets
        quantizer = faiss.IndexFlatIP(dim)
        self._index = faiss.IndexIVFFlat(quantizer, dim, 100)
        
    def add(self, vectors):
        self._index.train(vectors)
        self._index.add(vectors)
    
    def search(self, query, k):
        return self._index.search(query, k)
```

### Benchmark Target

| Method | QPS (1T) | QPS (8T) | Effort |
|--------|----------|----------|--------|
| Pure Python | 156 | 41 | - |
| NumPy batch | 500 | 400 | Low |
| Numba JIT | 2,000 | 12,000 | Medium |
| FAISS | 50,000 | 200,000 | High |

## 1.4 Phase 3: GPU Acceleration (Week 4-6)

### Goal
Offload vector operations to GPU for 100x+ throughput on large datasets.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    GPU-Accelerated Pipeline                 │
├─────────────────────────────────────────────────────────────┤
│  CPU Control Plane                                          │
│  ├── Query batching (collect N queries)                    │
│  ├── GPU memory management                                  │
│  └── Result post-processing                                 │
├─────────────────────────────────────────────────────────────┤
│  GPU Data Plane (CUDA/Metal)                                │
│  ├── cuBLAS/MLX distance computation                       │
│  ├── GPU-resident HNSW graph (for huge datasets)           │
│  └── Fused search kernel                                    │
├─────────────────────────────────────────────────────────────┤
│  Memory Transfer                                            │
│  ├── Pinned host memory for async copy                     │
│  ├── CUDA streams for overlapped compute/transfer          │
│  └── Unified memory (for M1/M2/M3 Macs)                    │
└─────────────────────────────────────────────────────────────┘
```

### Implementation (macOS Metal/MLX)

```python
import mlx.core as mx

class MLXVectorEngine:
    def __init__(self, dim):
        self._dim = dim
        self._vectors = None  # GPU-resident
    
    def load_vectors(self, vectors: np.ndarray):
        # Transfer to GPU (M1/M2/M3 unified memory)
        self._vectors = mx.array(vectors)
    
    def batch_search(self, queries: np.ndarray, k: int):
        q = mx.array(queries)  # (B, D)
        v = self._vectors      # (N, D)
        
        # GPU matrix multiply for all distances
        dots = mx.matmul(q, v.T)  # (B, N)
        norms_q = mx.linalg.norm(q, axis=1, keepdims=True)
        norms_v = mx.linalg.norm(v, axis=1)
        cos_sim = dots / (norms_q * norms_v + 1e-10)
        
        # GPU top-k selection
        _, indices = mx.topk(cos_sim, k, axis=1)
        return indices.tolist()
```

### Implementation (Linux CUDA)

```python
import cupy as cp
from cuml.neighbors import NearestNeighbors

class CUDAVectorEngine:
    def __init__(self, dim):
        self._nn = NearestNeighbors(
            n_neighbors=10,
            metric='cosine',
            algorithm='brute',  # or 'ivfflat' for large scale
        )
    
    def fit(self, vectors):
        self._nn.fit(cp.asarray(vectors))
    
    def search(self, queries, k):
        q_gpu = cp.asarray(queries)
        distances, indices = self._nn.kneighbors(q_gpu, k)
        return cp.asnumpy(indices), cp.asnumpy(distances)
```

### Performance Targets

| Scale | CPU (8T) | GPU (M2 Max) | GPU (A100) |
|-------|----------|--------------|------------|
| 10K vectors | 200 QPS | 5,000 QPS | 20,000 QPS |
| 100K vectors | 50 QPS | 2,000 QPS | 10,000 QPS |
| 1M vectors | 5 QPS | 500 QPS | 5,000 QPS |
| 10M vectors | N/A | 50 QPS | 1,000 QPS |

## 1.5 Phase 4: Distributed Parallelism (Week 7-8)

### Goal
Scale beyond single-machine limits with data sharding.

### Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                 Distributed Vector Search                   │
├─────────────────────────────────────────────────────────────┤
│  Coordinator Node                                           │
│  ├── Query routing (consistent hashing)                    │
│  ├── Result merging (parallel heap)                        │
│  └── Load balancing                                         │
├─────────────────────────────────────────────────────────────┤
│  Shard Nodes (N replicas)                                  │
│  ├── Local HNSW index                                      │
│  ├── GPU acceleration (optional)                           │
│  └── gRPC/TCP communication                                │
└─────────────────────────────────────────────────────────────┘
```

### Sharding Strategy

1. **Hash Sharding**: Distribute vectors by hash(id) % num_shards
2. **Space Partitioning**: K-means clustering, then shard by centroid
3. **Hybrid**: Hash for uniform load + replicas for fault tolerance

---

# Part 2: Python-to-C Migration Roadmap

## 2.1 Current Python Performance Profile

### Hot Path Analysis (via cProfile)

| Function | % Time | Calls | Reason |
|----------|--------|-------|--------|
| `_search_layer()` | 45% | 50K | Graph traversal |
| `_dist_fn()` | 30% | 500K | Distance computation |
| `RWLock.read()` | 15% | 1K | Lock overhead |
| `struct.pack/unpack` | 5% | 10K | IPC serialization |
| Other | 5% | - | - |

### Python Interpreter Overhead

```
Pure Python loop:     ~100 million ops/sec
NumPy vectorized:     ~1 billion ops/sec
C extension:          ~10 billion ops/sec
```

## 2.2 Migration Strategy

### Tier 1: Cython for Hot Paths (Week 1-2)

#### Target Files
```
qm_core/index/hnsw.py          → hnsw.pyx
qm_core/compression/xor_delta.py → xor_delta.pyx
qm_core/ipc/ring_buffer.py     → ring_buffer.pyx
```

#### Example: HNSW Distance Computation

```cython
# hnsw_simd.pyx
# cython: language_level=3
# cython: boundscheck=False
# cython: wraparound=False

import numpy as np
cimport numpy as cnp
from libc.math cimport sqrt

cpdef float cosine_distance(
    cnp.float32_t[:] a,
    cnp.float32_t[:] b,
) nogil:
    cdef:
        int i
        int n = a.shape[0]
        float dot = 0.0
        float na = 0.0
        float nb = 0.0
    
    for i in range(n):
        dot += a[i] * b[i]
        na += a[i] * a[i]
        nb += b[i] * b[i]
    
    na = sqrt(na)
    nb = sqrt(nb)
    if na < 1e-10 or nb < 1e-10:
        return 1.0
    return 1.0 - dot / (na * nb)


cpdef void batch_distances(
    cnp.float32_t[:] query,
    cnp.float32_t[:, :] vectors,
    cnp.float32_t[:] out,
) nogil:
    cdef int i
    for i in range(vectors.shape[0]):
        out[i] = cosine_distance(query, vectors[i, :])
```

#### Build Configuration

```python
# setup.py
from setuptools import setup
from Cython.Build import cythonize
import numpy as np

setup(
    ext_modules=cythonize([
        "qm_core/index/hnsw_simd.pyx",
        "qm_core/compression/xor_delta_fast.pyx",
    ]),
    include_dirs=[np.get_include()],
)
```

### Tier 2: C Extension with Python Bindings (Week 3-4)

#### Target: Complete HNSW Implementation in C

```c
// hnsw.c
#include <Python.h>
#include <numpy/arrayobject.h>
#include <stdlib.h>
#include <math.h>
#include <immintrin.h>  // AVX2

typedef struct {
    int id;
    float* vector;
    int* neighbors;
    int neighbor_count;
} HNSWNode;

typedef struct {
    HNSWNode* nodes;
    int node_count;
    int dim;
    int M;
    int ef_construction;
} HNSWIndex;

// AVX2-optimized cosine distance
float cosine_distance_avx2(const float* a, const float* b, int dim) {
    __m256 dot_vec = _mm256_setzero_ps();
    __m256 na_vec = _mm256_setzero_ps();
    __m256 nb_vec = _mm256_setzero_ps();
    
    for (int i = 0; i < dim; i += 8) {
        __m256 av = _mm256_loadu_ps(a + i);
        __m256 bv = _mm256_loadu_ps(b + i);
        dot_vec = _mm256_fmadd_ps(av, bv, dot_vec);
        na_vec = _mm256_fmadd_ps(av, av, na_vec);
        nb_vec = _mm256_fmadd_ps(bv, bv, nb_vec);
    }
    
    // Horizontal sum
    float dot = hsum_avx2(dot_vec);
    float na = sqrtf(hsum_avx2(na_vec));
    float nb = sqrtf(hsum_avx2(nb_vec));
    
    return 1.0f - dot / (na * nb + 1e-10f);
}

// Python binding
static PyObject* py_search(PyObject* self, PyObject* args) {
    PyArrayObject* query_arr;
    int top_k;
    
    if (!PyArg_ParseTuple(args, "O!i", &PyArray_Type, &query_arr, &top_k))
        return NULL;
    
    // ... search implementation ...
    
    return result_list;
}

static PyMethodDef HNSWMethods[] = {
    {"search", py_search, METH_VARARGS, "Search HNSW index"},
    {NULL, NULL, 0, NULL}
};

static struct PyModuleDef hnswmodule = {
    PyModuleDef_HEAD_INIT,
    "qm_hnsw_c",
    NULL,
    -1,
    HNSWMethods
};

PyMODINIT_FUNC PyInit_qm_hnsw_c(void) {
    import_array();
    return PyModule_Create(&hnswmodule);
}
```

### Tier 3: Rust with PyO3 (Week 5-6)

#### Advantages
- Memory safety guarantees
- Zero-cost abstractions
- Excellent concurrency primitives (no GIL equivalent)
- Mature ecosystem (rayon for parallelism)

#### Example: Rust HNSW

```rust
// src/hnsw.rs
use pyo3::prelude::*;
use numpy::{PyArray1, PyArray2, PyReadonlyArray1, PyReadonlyArray2};
use rayon::prelude::*;

#[pyclass]
struct HNSWIndex {
    vectors: Vec<Vec<f32>>,
    graph: Vec<Vec<usize>>,
    dim: usize,
}

#[pymethods]
impl HNSWIndex {
    #[new]
    fn new(dim: usize) -> Self {
        HNSWIndex {
            vectors: Vec::new(),
            graph: Vec::new(),
            dim,
        }
    }
    
    fn add(&mut self, id: usize, vector: PyReadonlyArray1<f32>) {
        let vec: Vec<f32> = vector.as_slice().unwrap().to_vec();
        self.vectors.push(vec);
        // ... build graph ...
    }
    
    fn batch_search<'py>(
        &self,
        py: Python<'py>,
        queries: PyReadonlyArray2<f32>,
        k: usize,
    ) -> &'py PyArray2<i64> {
        let queries = queries.as_array();
        let n_queries = queries.nrows();
        
        // Parallel search with Rayon
        let results: Vec<Vec<i64>> = (0..n_queries)
            .into_par_iter()
            .map(|i| {
                let q = queries.row(i).to_vec();
                self.search_one(&q, k)
            })
            .collect();
        
        // Convert to numpy
        PyArray2::from_vec2(py, &results).unwrap()
    }
}

#[pymodule]
fn qm_hnsw_rs(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<HNSWIndex>()?;
    Ok(())
}
```

#### Cargo.toml

```toml
[package]
name = "qm_hnsw_rs"
version = "0.1.0"
edition = "2021"

[lib]
name = "qm_hnsw_rs"
crate-type = ["cdylib"]

[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
numpy = "0.20"
rayon = "1.8"
```

## 2.3 Migration Checklist

### Phase 1: Cython (2 weeks)
- [ ] Profile Python code to identify hotspots
- [ ] Convert `hnsw.py` → `hnsw.pyx`
- [ ] Convert `xor_delta.py` → `xor_delta.pyx`
- [ ] Add CI/CD for wheel building
- [ ] Benchmark: Target 5-10x speedup

### Phase 2: C Extension (2 weeks)
- [ ] Implement AVX2/NEON SIMD kernels
- [ ] Memory-map vector storage
- [ ] Lock-free graph traversal
- [ ] Benchmark: Target 20-50x speedup

### Phase 3: Rust (2 weeks)
- [ ] Port hot paths to Rust with PyO3
- [ ] Implement concurrent search with Rayon
- [ ] Zero-copy numpy integration
- [ ] Benchmark: Target 50-100x speedup

## 2.4 Expected Performance Gains

| Stage | Implementation | Single-Thread | Multi-Thread (8T) |
|-------|----------------|---------------|-------------------|
| Baseline | Pure Python | 156 QPS | 41 QPS |
| Cython | Type annotations | 800 QPS | 2,000 QPS |
| C + AVX2 | SIMD kernels | 5,000 QPS | 30,000 QPS |
| Rust + Rayon | Lock-free parallel | 10,000 QPS | 70,000 QPS |
| Rust + GPU | Metal/CUDA | 50,000 QPS | N/A |

---

# Part 3: Implementation Priority Matrix

| Task | Impact | Effort | Priority |
|------|--------|--------|----------|
| Fix RWLock contention | Critical | Low | P0 |
| Batch distance computation | High | Low | P0 |
| Cython hot paths | High | Medium | P1 |
| FAISS integration | Very High | Medium | P1 |
| GPU (MLX) backend | Very High | High | P2 |
| Rust migration | High | High | P2 |
| Distributed sharding | Medium | Very High | P3 |

## Recommended 8-Week Sprint Plan

| Week | Focus | Deliverable |
|------|-------|-------------|
| 1 | Lock-free reads | RCU pattern for HNSW |
| 2 | SIMD distances | NumPy batch + Numba JIT |
| 3 | Cython core | hnsw.pyx, xor_delta.pyx |
| 4 | FAISS integration | Optional backend |
| 5 | C extension | AVX2 distance kernels |
| 6 | GPU backend | MLX for macOS |
| 7 | Rust port | PyO3 bindings |
| 8 | Benchmarking | Full performance suite |

---

# Appendix A: Benchmark Commands

```bash
# Local-mode vector search benchmark
python benchmarks/local_vector_search_bench.py \
    --num-vectors 100000 \
    --dim 128 \
    --num-queries 10000 \
    --output benchmarks/results/local_100k.md

# Compare with FAISS baseline
python benchmarks/faiss_comparison.py \
    --num-vectors 100000 \
    --dim 128

# GPU benchmark (macOS)
python benchmarks/gpu_mlx_bench.py \
    --num-vectors 1000000 \
    --batch-size 1000
```

# Appendix B: Profiling Commands

```bash
# CPU profiling
python -m cProfile -s cumtime -o profile.pstats benchmarks/local_vector_search_bench.py
snakeviz profile.pstats

# Memory profiling
python -m memory_profiler benchmarks/local_vector_search_bench.py

# Line-by-line profiling
kernprof -l -v qm_core/index/hnsw.py
```
