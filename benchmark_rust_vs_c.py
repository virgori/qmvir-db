#!/usr/bin/env python3
"""Benchmark: Rust vs C vs Python for vector operations."""

import numpy as np
import time
import sys

# Setup
np.random.seed(42)
DIM = 128
N_VECTORS = 10000
N_QUERIES = 100
ITERATIONS = 3

vectors = np.random.rand(N_VECTORS, DIM).astype(np.float32)
queries = np.random.rand(N_QUERIES, DIM).astype(np.float32)

print("=" * 60)
print("BENCHMARK: Rust vs C vs Python/NumPy")
print("=" * 60)
print(f"Vectors: {N_VECTORS}, Dimension: {DIM}, Queries: {N_QUERIES}")
print(f"Iterations: {ITERATIONS}")
print()

results = {}

# =============================================================================
# Rust Native (qm_native)
# =============================================================================
try:
    from qm_native import cosine_distance as rust_cosine, batch_cosine_distances as rust_batch
    
    # Warmup
    for _ in range(10):
        rust_batch(queries[0], vectors)
    
    # Benchmark single distance
    times = []
    for _ in range(ITERATIONS):
        t0 = time.perf_counter()
        for i in range(1000):
            rust_cosine(queries[i % N_QUERIES], vectors[i % N_VECTORS])
        times.append(time.perf_counter() - t0)
    rust_single = min(times)
    results['Rust single (1k calls)'] = rust_single * 1000
    
    # Benchmark batch
    times = []
    for _ in range(ITERATIONS):
        t0 = time.perf_counter()
        for q in queries:
            _ = rust_batch(q, vectors)
        times.append(time.perf_counter() - t0)
    rust_batch_time = min(times)
    results['Rust batch (100 x 10k)'] = rust_batch_time * 1000
    
    print(f"Rust:")
    print(f"  Single distance (1000 calls): {rust_single*1000:.2f}ms")
    print(f"  Batch distance (100 x 10k):   {rust_batch_time*1000:.2f}ms")
    
except ImportError as e:
    print(f"Rust: NOT AVAILABLE ({e})")

print()

# =============================================================================
# C Native (qm_native_c)
# =============================================================================
try:
    from qm_native_c import cosine_distance as c_cosine, batch_cosine_distances as c_batch
    
    # Warmup
    for _ in range(10):
        c_batch(queries[0], vectors)
    
    # Benchmark single distance
    times = []
    for _ in range(ITERATIONS):
        t0 = time.perf_counter()
        for i in range(1000):
            c_cosine(queries[i % N_QUERIES], vectors[i % N_VECTORS])
        times.append(time.perf_counter() - t0)
    c_single = min(times)
    results['C single (1k calls)'] = c_single * 1000
    
    # Benchmark batch
    times = []
    for _ in range(ITERATIONS):
        t0 = time.perf_counter()
        for q in queries:
            _ = c_batch(q, vectors)
        times.append(time.perf_counter() - t0)
    c_batch_time = min(times)
    results['C batch (100 x 10k)'] = c_batch_time * 1000
    
    print(f"C (NEON SIMD):")
    print(f"  Single distance (1000 calls): {c_single*1000:.2f}ms")
    print(f"  Batch distance (100 x 10k):   {c_batch_time*1000:.2f}ms")
    
except ImportError as e:
    print(f"C: NOT AVAILABLE ({e})")

print()

# =============================================================================
# Pure Python/NumPy
# =============================================================================
def py_cosine(a, b):
    dot = np.dot(a, b)
    na, nb = np.linalg.norm(a), np.linalg.norm(b)
    return 1.0 - dot / (na * nb + 1e-10)

def py_batch_cosine(query, vecs):
    dots = vecs @ query
    norms_v = np.linalg.norm(vecs, axis=1)
    norm_q = np.linalg.norm(query)
    return 1.0 - dots / (norms_v * norm_q + 1e-10)

# Warmup
for _ in range(10):
    py_batch_cosine(queries[0], vectors)

# Benchmark single
times = []
for _ in range(ITERATIONS):
    t0 = time.perf_counter()
    for i in range(1000):
        py_cosine(queries[i % N_QUERIES], vectors[i % N_VECTORS])
    times.append(time.perf_counter() - t0)
py_single = min(times)
results['NumPy single (1k calls)'] = py_single * 1000

# Benchmark batch
times = []
for _ in range(ITERATIONS):
    t0 = time.perf_counter()
    for q in queries:
        _ = py_batch_cosine(q, vectors)
    times.append(time.perf_counter() - t0)
py_batch_time = min(times)
results['NumPy batch (100 x 10k)'] = py_batch_time * 1000

print(f"Python/NumPy:")
print(f"  Single distance (1000 calls): {py_single*1000:.2f}ms")
print(f"  Batch distance (100 x 10k):   {py_batch_time*1000:.2f}ms")

print()

# =============================================================================
# Summary
# =============================================================================
print("=" * 60)
print("SUMMARY")
print("=" * 60)

# Print comparison table
baseline = py_batch_time * 1000
print(f"\nBatch Distance (100 queries x 10k vectors):")
print(f"  {'Implementation':<20} {'Time (ms)':<12} {'Speedup':<10}")
print(f"  {'-'*20} {'-'*12} {'-'*10}")

for name, ms in sorted(results.items(), key=lambda x: x[1]):
    if 'batch' in name.lower():
        speedup = baseline / ms
        print(f"  {name.split()[0]:<20} {ms:<12.2f} {speedup:.2f}x")

print()
print("Winner:", min([(k, v) for k, v in results.items() if 'batch' in k.lower()], key=lambda x: x[1])[0].split()[0])
