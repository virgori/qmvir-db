#!/usr/bin/env python3
"""Benchmark multi-threaded HNSW batch search - Rust vs Python."""

import numpy as np
import time

from qm_native import HNSWIndex, batch_cosine_distances

# Setup
np.random.seed(42)
DIM = 128
N_VECTORS = 5000
N_QUERIES = 100

vectors = np.random.rand(N_VECTORS, DIM).astype(np.float32)
queries = np.random.rand(N_QUERIES, DIM).astype(np.float32)

print('=== Building Rust HNSW Index ===')
index = HNSWIndex(DIM, m=16, ef_construction=100)
t0 = time.perf_counter()
for i, v in enumerate(vectors):
    index.add(i, v)
build_time = time.perf_counter() - t0
print(f'Built index with {N_VECTORS} vectors in {build_time*1000:.0f}ms')

print('\n=== Single-threaded Search ===')
t0 = time.perf_counter()
for q in queries:
    _ = index.search(q, top_k=10)
single_time = time.perf_counter() - t0
print(f'Sequential 100 queries: {single_time*1000:.1f}ms')

print('\n=== Rayon Parallel Search (batch_search) ===')
# batch_search uses Rayon internally
t0 = time.perf_counter()
query_list = [q for q in queries]  # Convert to list
results = index.batch_search(query_list, top_k=10)
parallel_time = time.perf_counter() - t0
print(f'Parallel 100 queries: {parallel_time*1000:.1f}ms')
print(f'Parallel speedup: {single_time/parallel_time:.1f}x')

print('\n=== Summary ===')
print(f'Single-thread: {single_time*1000:.1f}ms ({100/single_time:.0f} qps)')
print(f'Multi-thread:  {parallel_time*1000:.1f}ms ({100/parallel_time:.0f} qps)')
print(f'Speedup: {single_time/parallel_time:.1f}x')
