#!/usr/bin/env python3
"""Benchmark Rust native extension vs Python."""

import numpy as np
import time

# Rust native
from qm_native import cosine_distance, batch_cosine_distances, HNSWIndex

# Setup
np.random.seed(42)
DIM = 128
N_VECTORS = 10000
N_QUERIES = 100

vectors = np.random.rand(N_VECTORS, DIM).astype(np.float32)
queries = np.random.rand(N_QUERIES, DIM).astype(np.float32)

# --- Benchmark batch_cosine_distances ---
print('=== Distance Computation ===')
t0 = time.perf_counter()
for q in queries:
    _ = batch_cosine_distances(q, vectors)
rust_time = time.perf_counter() - t0
print(f'Rust batch_cosine (100 queries x 10k vecs): {rust_time*1000:.1f}ms')

# Python version
t0 = time.perf_counter()  
for q in queries:
    norms_q = np.linalg.norm(q)
    norms_v = np.linalg.norm(vectors, axis=1)
    dots = vectors @ q
    _ = 1 - dots / (norms_q * norms_v + 1e-8)
py_time = time.perf_counter() - t0
print(f'Python batch_cosine (100 queries x 10k vecs): {py_time*1000:.1f}ms')
print(f'Speedup: {py_time/rust_time:.1f}x')

# --- Benchmark HNSW ---
print('\n=== HNSW Index ===')
index = HNSWIndex(DIM, m=16, ef_construction=100)

# Insert
t0 = time.perf_counter()
for i, v in enumerate(vectors[:1000]):
    index.add(i, v)
insert_time = time.perf_counter() - t0
print(f'Rust HNSW insert 1000 vecs: {insert_time*1000:.1f}ms')

# Search
t0 = time.perf_counter()
for q in queries[:10]:
    _ = index.search(q, top_k=10)
search_time = time.perf_counter() - t0
print(f'Rust HNSW search 10 queries: {search_time*1000:.1f}ms')
print(f'Avg search: {search_time/10*1000:.2f}ms')

print('\n✓ Native extension benchmark complete')
