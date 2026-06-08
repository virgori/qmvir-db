"""QM Core — Ultra-modern database kernel.

Architecture:
    storage/    — WAL, segments, buffer pool, MVCC, compaction
    index/      — B+tree, roaring bitmap, inverted+BMW, HNSW+PQ
    execution/  — parser, plan nodes, cost-based planner, vectorized engine
    statistics/ — cost model, HLL, CMS, t-digest
    optimizer/  — adaptive query execution
    learned/    — selectivity, cache, fusion assistants
    native/     — C/SIMD kernels for hot paths
"""

__version__ = "1.0.0"
