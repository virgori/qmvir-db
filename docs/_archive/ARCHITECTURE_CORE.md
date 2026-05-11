# QM Database Core — Architecture (v2.0 Ultra-Modern)

## Overview

QM is an ultra-modern multi-engine database core built from scratch in Python
with optional C extensions for performance-critical paths. It follows a
**3-layer kernel architecture**:

```
┌─────────────────────────────────────────────────────────────────┐
│                         QM Engine                                │
│   execute(DSL) → parse → plan → optimize → vectorized execute   │
├─────────────────────────────────────────────────────────────────┤
│                    Execution Kernel                               │
│   Query Parser │ Cost-Based Planner │ Vectorized Engine          │
│   Multi-Stage Pipeline │ Late Materialization                    │
├─────────────────────────────────────────────────────────────────┤
│                      Index Kernel                                │
│   B+Tree │ Roaring Bitmap │ Inverted (BMW/WAND)                 │
│   HNSW + Product Quantization │ Statistics                       │
├─────────────────────────────────────────────────────────────────┤
│                     Storage Kernel                               │
│   Binary WAL │ Segment Manager │ Buffer Pool (LRU)              │
│   MVCC (Snapshot Isolation) │ Tiered Compaction                  │
└─────────────────────────────────────────────────────────────────┘
```

Plus three assistive layers:
- **Statistics**: Cost model + probabilistic sketches (HLL, CMS, T-Digest, Bloom)
- **Optimizer**: Adaptive query execution with rewrite rules
- **Learned**: Selectivity correction, cache prediction, fusion weights, intent classification

## Directory Structure

```
qm_core/
├── __init__.py              # Package root, version
├── engine.py                # Top-level engine (ties everything together)
│
├── storage/                 # Layer 1: Storage Kernel
│   ├── wal.py               #   Binary WAL with CRC32, checkpointing
│   ├── segments.py          #   Segment format (8KB pages, slot arrays)
│   ├── buffer_pool.py       #   LRU buffer pool with pin/unpin
│   ├── mvcc.py              #   MVCC with version chains, snapshot isolation
│   └── compaction.py        #   Leveled / size-tiered / FIFO compaction
│
├── index/                   # Layer 2: Index Kernel
│   ├── btree.py             #   B+tree with leaf chains, range scan, bulk load
│   ├── roaring.py           #   Roaring bitmap (Array/Bitmap/Run containers)
│   ├── inverted.py          #   Inverted index + Block-Max WAND + WAND + DAAT
│   ├── hnsw.py              #   HNSW graph + Product Quantization + Two-Stage ANN
│   └── stats.py             #   Column/index/table statistics for planner
│
├── execution/               # Layer 3: Execution Kernel
│   ├── parser.py            #   Query DSL → AST (find, search, vector, hybrid, agg)
│   ├── plan.py              #   Logical + Physical plan node types
│   ├── planner.py           #   Cost-based query planner (access path selection)
│   ├── vectorized.py        #   Vectorized batch engine (numpy-backed)
│   └── pipeline.py          #   Multi-stage retrieval pipeline
│
├── statistics/              # Assistive: Statistics & Cost Model
│   ├── cost_model.py        #   I/O + CPU cost model for every operator
│   └── sketches.py          #   HyperLogLog, Count-Min Sketch, T-Digest, Bloom Filter
│
├── optimizer/               # Assistive: Adaptive Optimizer
│   └── adaptive.py          #   Cardinality fences, rule rewrites, plan history
│
├── learned/                 # Assistive: Learned Components
│   └── assistants.py        #   Selectivity, cache policy, fusion weights, intent
│
└── native/                  # Optional: C Extensions
    ├── qm_native.c          #   SIMD bitmap ops, BM25 block scoring, L2 distance, CRC32
    └── setup.py             #   Build script
```

## Key Algorithms

### Storage Kernel

| Component | Algorithm | Guarantee |
|-----------|-----------|-----------|
| WAL | Binary records + CRC32 | Crash recovery, durability |
| Segments | 8KB pages + slot arrays | Immutable after seal |
| Buffer Pool | LRU with pin counting | Controlled memory usage |
| MVCC | Version chains + snapshots | Snapshot isolation |
| Compaction | Leveled/Size-Tiered/FIFO | Space reclamation |

### Index Kernel

| Component | Algorithm | Complexity |
|-----------|-----------|------------|
| B+Tree | Balanced tree with leaf chain | O(log n) insert/lookup |
| Roaring Bitmap | Array/Bitmap/Run containers | Sublinear set ops |
| Inverted Index | Block-Max WAND (BMW) | ~20-30% postings touched |
| HNSW | Multi-layer proximity graph | O(log n) × ef_search |
| Product Quantization | Subspace quantization | ~100× compression |

### Execution Kernel

| Component | Technique | Benefit |
|-----------|-----------|---------|
| Parser | DSL → AST with type system | Unified query language |
| Planner | Cost-based (I/O + CPU model) | Optimal access paths |
| Vectorized Engine | Batch 256-1024 rows, numpy | 10-50× over row-at-a-time |
| Pipeline | Multi-stage with budgets | Prune early, score late |
| Late Materialization | ID-first, data-last | Minimal I/O for top-k |

### Statistics & Sketches

| Sketch | Purpose | Error |
|--------|---------|-------|
| HyperLogLog (p=14) | Cardinality estimation | ~0.81% |
| Count-Min Sketch | Frequency estimation | ε ≈ 0.0013 |
| T-Digest (δ=100) | Quantile estimation | <1% at tails |
| Bloom Filter | Set membership | Configurable FP rate |

### Learned Components

| Component | Method | Fallback |
|-----------|--------|----------|
| Selectivity | EMA correction factors | Traditional estimation |
| Cache Policy | Access interval EMA | LRU eviction |
| Fusion Weights | Per-intent alpha tuning | α = 0.5 |
| Intent Classifier | Feature-based heuristics | Hybrid default |

## Design Principles

1. **Binary over JSON**: All storage formats use binary encoding with checksums
2. **Batch over Row**: Vectorized execution processes 256-1024 rows per batch
3. **Late over Early**: Materialize full documents only for final top-k results
4. **Adaptive over Static**: Runtime monitoring triggers re-optimization
5. **Approximate before Exact**: Use sketches and PQ for initial filtering
6. **Block-Max over Full-Scan**: BMW skips 70-80% of postings in search

## Anti-Patterns Avoided

- ❌ JSON WAL → ✅ Binary WAL with CRC32
- ❌ Python set as bitmap → ✅ Roaring Bitmap with typed containers
- ❌ Row-at-a-time execution → ✅ Vectorized batch execution
- ❌ Full document fetch for scoring → ✅ Late materialization
- ❌ Fixed query plans → ✅ Cost-based + adaptive planning
- ❌ Single search algorithm → ✅ BMW / WAND / DAAT selection

## Performance Targets

Run `python tools/benchmark.py` to measure actual performance.

| Operation | Target |
|-----------|--------|
| B+Tree point lookup | >500K ops/s |
| Roaring bitmap AND (1M) | <1ms |
| BMW search (10K docs) | <5ms/query |
| HNSW search (5K vecs) | <10ms/query |
| Vectorized filter (100K) | <5ms |
| Engine insert | >10K ops/s |

## Usage

```python
from qm_core.engine import QMEngine

engine = QMEngine("/path/to/data")

engine.create_table("docs", schema={
    "title": "text", "body": "text", "views": "int",
}, vector_dim=384)

engine.insert("docs", {"title": "Hello", "body": "World", "views": 42})

engine.create_index("docs", "views_idx", ["views"], index_type="btree")
engine.create_index("docs", "text_idx", ["title", "body"], index_type="inverted")
engine.create_index("docs", "vec_idx", ["embedding"], index_type="hnsw")

results = engine.find("docs", predicates=[{"column": "views", "op": "gt", "value": 10}])
results = engine.search("docs", query="database systems", top_k=10)
results = engine.hybrid_search("docs", query="modern db", vector=[0.1, ...], top_k=10)
results = engine.aggregate("docs", group_by=["category"],
                           aggregates=[("count", "*", "cnt")])

print(engine.explain({"action": "find", "table": "docs"}))
engine.analyze("docs")
print(engine.stats())
```
