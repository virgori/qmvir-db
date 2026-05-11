#!/usr/bin/env python3
"""QMvir Local-Mode Vector Search Benchmark.

Tests vector search performance using direct Python API (no network round-trip).

Benchmarks:
    1. Vector insertion: bulk insert with HNSW index building
    2. HNSW ANN search: approximate nearest neighbors
    3. Brute-force search: exact exhaustive search (baseline)
    4. XOR-Delta compression: lossless vector compression overhead

Metrics collected:
    - Throughput (ops/sec)
    - Latency (avg, p50, p95, p99)
    - Recall@K (vs brute-force ground truth)
    - Compression ratio (for XOR-Delta)
"""

from __future__ import annotations

import argparse
import multiprocessing
import os
import statistics
import sys
import time
from dataclasses import dataclass
from pathlib import Path

import numpy as np

# Ensure QM modules are importable
QM_ROOT = Path(__file__).resolve().parent.parent
if str(QM_ROOT) not in sys.path:
    sys.path.insert(0, str(QM_ROOT))

from qm_core.hub_engine import QMHubEngine
from qm_core.satellite.vector_satellite import VectorSatellite
from qm_core.satellite.base import SatelliteConfig
from qm_core.index.hnsw import HNSWIndex, Metric


@dataclass
class BenchmarkConfig:
    """Benchmark configuration."""
    num_vectors: int = 10000
    dim: int = 128
    num_queries: int = 1000
    top_k: int = 10
    ef_search: int = 50
    seed: int = 42
    output_file: str | None = None


def generate_vectors(n: int, dim: int, seed: int = 42) -> np.ndarray:
    """Generate random unit-normalized vectors."""
    rng = np.random.default_rng(seed)
    vecs = rng.standard_normal((n, dim)).astype(np.float32)
    # Normalize to unit vectors for cosine distance
    norms = np.linalg.norm(vecs, axis=1, keepdims=True)
    norms[norms < 1e-10] = 1.0
    return vecs / norms


def brute_force_search(
    query: np.ndarray,
    vectors: np.ndarray,
    top_k: int,
) -> list[tuple[int, float]]:
    """Exact brute-force k-NN search (ground truth)."""
    # Cosine distance = 1 - cosine_similarity
    dot = vectors @ query
    norms_q = np.linalg.norm(query)
    norms_v = np.linalg.norm(vectors, axis=1)
    cos_sim = dot / (norms_v * norms_q + 1e-10)
    dists = 1.0 - cos_sim
    indices = np.argsort(dists)[:top_k]
    return [(int(i), float(dists[i])) for i in indices]


def compute_recall(
    pred: list[tuple[int, float]],
    truth: list[tuple[int, float]],
    k: int,
) -> float:
    """Compute Recall@K."""
    pred_ids = set(p[0] for p in pred[:k])
    truth_ids = set(t[0] for t in truth[:k])
    if len(truth_ids) == 0:
        return 1.0
    return len(pred_ids & truth_ids) / len(truth_ids)


class VectorSearchBenchmark:
    """Local-mode vector search benchmark suite."""

    def __init__(self, config: BenchmarkConfig):
        self.config = config
        self.vectors: np.ndarray | None = None
        self.queries: np.ndarray | None = None
        self.hnsw: HNSWIndex | None = None
        self.results: dict = {}

    def setup(self) -> None:
        """Generate test data."""
        print(f"Generating {self.config.num_vectors} vectors (dim={self.config.dim})...")
        self.vectors = generate_vectors(
            self.config.num_vectors,
            self.config.dim,
            self.config.seed,
        )
        print(f"Generating {self.config.num_queries} query vectors...")
        self.queries = generate_vectors(
            self.config.num_queries,
            self.config.dim,
            self.config.seed + 1000,
        )

    def bench_hnsw_build(self) -> dict:
        """Benchmark HNSW index construction."""
        print("\n[1] HNSW Index Build...")
        self.hnsw = HNSWIndex(dim=self.config.dim, metric=Metric.COSINE, M=16, ef_construction=200)

        latencies = []
        t_start = time.perf_counter()
        for i, vec in enumerate(self.vectors):
            t0 = time.perf_counter()
            self.hnsw.add(i, vec)
            latencies.append((time.perf_counter() - t0) * 1000.0)
            if (i + 1) % 2000 == 0:
                print(f"  Inserted {i+1}/{self.config.num_vectors}...")
        elapsed = time.perf_counter() - t_start

        result = {
            "operation": "hnsw_build",
            "num_vectors": self.config.num_vectors,
            "dim": self.config.dim,
            "elapsed_s": round(elapsed, 3),
            "throughput_ops": round(self.config.num_vectors / elapsed, 2),
            "avg_latency_ms": round(statistics.mean(latencies), 3),
            "p50_latency_ms": round(statistics.median(latencies), 3),
            "p95_latency_ms": round(statistics.quantiles(latencies, n=100)[94], 3),
            "p99_latency_ms": round(statistics.quantiles(latencies, n=100)[98], 3),
        }
        print(f"  Done: {result['throughput_ops']} ops/sec, avg={result['avg_latency_ms']}ms")
        return result

    def bench_hnsw_search(self) -> dict:
        """Benchmark HNSW ANN search."""
        print("\n[2] HNSW ANN Search...")
        if self.hnsw is None:
            print("  ERROR: HNSW index not built")
            return {}

        latencies = []
        recalls = []
        t_start = time.perf_counter()
        for q in self.queries:
            t0 = time.perf_counter()
            hnsw_results = self.hnsw.search(q, top_k=self.config.top_k, ef_search=self.config.ef_search)
            latencies.append((time.perf_counter() - t0) * 1000.0)
            pred = [(r.id, r.distance) for r in hnsw_results]
            truth = brute_force_search(q, self.vectors, self.config.top_k)
            recalls.append(compute_recall(pred, truth, self.config.top_k))
        elapsed = time.perf_counter() - t_start

        result = {
            "operation": "hnsw_search",
            "num_queries": self.config.num_queries,
            "top_k": self.config.top_k,
            "ef_search": self.config.ef_search,
            "elapsed_s": round(elapsed, 3),
            "throughput_qps": round(self.config.num_queries / elapsed, 2),
            "avg_latency_ms": round(statistics.mean(latencies), 3),
            "p50_latency_ms": round(statistics.median(latencies), 3),
            "p95_latency_ms": round(statistics.quantiles(latencies, n=100)[94], 3),
            "p99_latency_ms": round(statistics.quantiles(latencies, n=100)[98], 3),
            "recall_at_k": round(statistics.mean(recalls), 4),
        }
        print(f"  Done: {result['throughput_qps']} QPS, avg={result['avg_latency_ms']}ms, recall@{self.config.top_k}={result['recall_at_k']}")
        return result

    def bench_brute_force(self) -> dict:
        """Benchmark brute-force exact search (baseline)."""
        print("\n[3] Brute-Force Exact Search (baseline)...")
        latencies = []
        t_start = time.perf_counter()
        for q in self.queries:
            t0 = time.perf_counter()
            _ = brute_force_search(q, self.vectors, self.config.top_k)
            latencies.append((time.perf_counter() - t0) * 1000.0)
        elapsed = time.perf_counter() - t_start

        result = {
            "operation": "brute_force_search",
            "num_queries": self.config.num_queries,
            "top_k": self.config.top_k,
            "elapsed_s": round(elapsed, 3),
            "throughput_qps": round(self.config.num_queries / elapsed, 2),
            "avg_latency_ms": round(statistics.mean(latencies), 3),
            "p50_latency_ms": round(statistics.median(latencies), 3),
            "p95_latency_ms": round(statistics.quantiles(latencies, n=100)[94], 3),
            "p99_latency_ms": round(statistics.quantiles(latencies, n=100)[98], 3),
        }
        print(f"  Done: {result['throughput_qps']} QPS, avg={result['avg_latency_ms']}ms")
        return result

    def bench_xor_delta_compression(self) -> dict:
        """Benchmark XOR-Delta lossless compression."""
        print("\n[4] XOR-Delta Lossless Compression...")
        try:
            from qm_core.compression import XORDeltaCodec, XORDeltaBatchCodec
        except ImportError:
            print("  SKIP: XORDeltaCodec not available")
            return {}

        codec = XORDeltaCodec(self.config.dim)
        batch_codec = XORDeltaBatchCodec(self.config.dim)

        # Single-vector compression
        single_latencies = []
        compressed_sizes = []
        for vec in self.vectors[:1000]:
            t0 = time.perf_counter()
            compressed = codec.encode(vec)
            single_latencies.append((time.perf_counter() - t0) * 1000.0)
            compressed_sizes.append(len(compressed))

        # Batch compression
        t0 = time.perf_counter()
        batch_compressed = batch_codec.encode_batch(self.vectors)
        batch_time = time.perf_counter() - t0

        raw_bytes = self.vectors.nbytes
        avg_single_compressed = statistics.mean(compressed_sizes)
        batch_compressed_bytes = len(batch_compressed)

        result = {
            "operation": "xor_delta_compression",
            "num_vectors": self.config.num_vectors,
            "raw_bytes": raw_bytes,
            "single_avg_compressed_bytes": round(avg_single_compressed, 1),
            "single_compression_ratio": round(self.vectors[0].nbytes / avg_single_compressed, 2),
            "single_avg_latency_ms": round(statistics.mean(single_latencies), 4),
            "batch_compressed_bytes": batch_compressed_bytes,
            "batch_compression_ratio": round(raw_bytes / batch_compressed_bytes, 2),
            "batch_elapsed_s": round(batch_time, 3),
            "batch_throughput_mbs": round((raw_bytes / 1e6) / batch_time, 2),
        }
        print(f"  Done: batch ratio={result['batch_compression_ratio']}x, {result['batch_throughput_mbs']} MB/s")
        return result

    def bench_multi_threaded_search(self, num_threads: int = 4) -> dict:
        """Benchmark multi-threaded HNSW search."""
        print(f"\n[5] Multi-Threaded HNSW Search ({num_threads} threads)...")
        if self.hnsw is None:
            print("  ERROR: HNSW index not built")
            return {}

        from concurrent.futures import ThreadPoolExecutor

        queries_per_thread = self.config.num_queries // num_threads

        def worker(thread_id: int) -> list[float]:
            lats = []
            start = thread_id * queries_per_thread
            end = start + queries_per_thread
            for q in self.queries[start:end]:
                t0 = time.perf_counter()
                _ = self.hnsw.search(q, top_k=self.config.top_k, ef_search=self.config.ef_search)
                lats.append((time.perf_counter() - t0) * 1000.0)
            return lats

        t_start = time.perf_counter()
        all_latencies = []
        with ThreadPoolExecutor(max_workers=num_threads) as ex:
            futures = [ex.submit(worker, i) for i in range(num_threads)]
            for f in futures:
                all_latencies.extend(f.result())
        elapsed = time.perf_counter() - t_start

        total_queries = len(all_latencies)
        result = {
            "operation": "multi_threaded_hnsw_search",
            "num_threads": num_threads,
            "total_queries": total_queries,
            "elapsed_s": round(elapsed, 3),
            "throughput_qps": round(total_queries / elapsed, 2),
            "avg_latency_ms": round(statistics.mean(all_latencies), 3),
            "p50_latency_ms": round(statistics.median(all_latencies), 3),
            "p95_latency_ms": round(statistics.quantiles(all_latencies, n=100)[94], 3),
            "p99_latency_ms": round(statistics.quantiles(all_latencies, n=100)[98], 3),
        }
        print(f"  Done: {result['throughput_qps']} QPS ({num_threads} threads), avg={result['avg_latency_ms']}ms")
        return result

    def run_all(self) -> dict:
        """Run all benchmarks."""
        self.setup()

        results = {
            "config": {
                "num_vectors": self.config.num_vectors,
                "dim": self.config.dim,
                "num_queries": self.config.num_queries,
                "top_k": self.config.top_k,
                "ef_search": self.config.ef_search,
                "cpu_count": multiprocessing.cpu_count(),
            },
            "benchmarks": [],
        }

        results["benchmarks"].append(self.bench_hnsw_build())
        results["benchmarks"].append(self.bench_hnsw_search())
        results["benchmarks"].append(self.bench_brute_force())
        results["benchmarks"].append(self.bench_xor_delta_compression())

        # Multi-threaded scaling
        for nt in [1, 2, 4, 8]:
            if nt <= multiprocessing.cpu_count():
                results["benchmarks"].append(self.bench_multi_threaded_search(num_threads=nt))

        self.results = results
        return results


def format_report(results: dict) -> str:
    """Format benchmark results as markdown report."""
    lines = [
        "# QMvir Local-Mode Vector Search Benchmark Report",
        "",
        "## Configuration",
        f"- Vectors: {results['config']['num_vectors']:,}",
        f"- Dimensions: {results['config']['dim']}",
        f"- Queries: {results['config']['num_queries']:,}",
        f"- Top-K: {results['config']['top_k']}",
        f"- ef_search: {results['config']['ef_search']}",
        f"- CPU cores: {results['config']['cpu_count']}",
        "",
        "## Results Summary",
        "",
        "| Operation | Throughput | Avg Latency | P95 Latency | Notes |",
        "|---|---:|---:|---:|---|",
    ]

    for b in results["benchmarks"]:
        if not b:
            continue
        op = b.get("operation", "")
        if "throughput_ops" in b:
            tp = f"{b['throughput_ops']:,.0f} ops/s"
        elif "throughput_qps" in b:
            tp = f"{b['throughput_qps']:,.0f} QPS"
        elif "batch_throughput_mbs" in b:
            tp = f"{b['batch_throughput_mbs']:.1f} MB/s"
        else:
            tp = "-"

        avg = f"{b.get('avg_latency_ms', b.get('single_avg_latency_ms', '-'))} ms"
        p95 = f"{b.get('p95_latency_ms', '-')} ms"

        notes = ""
        if "recall_at_k" in b:
            notes = f"recall@{b.get('top_k', 10)}={b['recall_at_k']}"
        elif "batch_compression_ratio" in b:
            notes = f"ratio={b['batch_compression_ratio']}x"
        elif "num_threads" in b:
            notes = f"{b['num_threads']} threads"

        lines.append(f"| {op} | {tp} | {avg} | {p95} | {notes} |")

    lines.extend([
        "",
        "## Multi-Threading Scaling",
        "",
        "| Threads | QPS | Speedup vs 1T |",
        "|---:|---:|---:|",
    ])

    base_qps = None
    for b in results["benchmarks"]:
        if b.get("operation") == "multi_threaded_hnsw_search":
            qps = b["throughput_qps"]
            nt = b["num_threads"]
            if nt == 1:
                base_qps = qps
            speedup = qps / base_qps if base_qps else 1.0
            lines.append(f"| {nt} | {qps:,.0f} | {speedup:.2f}x |")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="QMvir Local-Mode Vector Search Benchmark")
    parser.add_argument("--num-vectors", type=int, default=10000, help="Number of vectors to index")
    parser.add_argument("--dim", type=int, default=128, help="Vector dimension")
    parser.add_argument("--num-queries", type=int, default=1000, help="Number of search queries")
    parser.add_argument("--top-k", type=int, default=10, help="Top-K results")
    parser.add_argument("--ef-search", type=int, default=50, help="HNSW ef_search parameter")
    parser.add_argument("--output", type=str, default=None, help="Output markdown file")
    args = parser.parse_args()

    config = BenchmarkConfig(
        num_vectors=args.num_vectors,
        dim=args.dim,
        num_queries=args.num_queries,
        top_k=args.top_k,
        ef_search=args.ef_search,
        output_file=args.output,
    )

    bench = VectorSearchBenchmark(config)
    results = bench.run_all()

    print("\n" + "=" * 60)
    report = format_report(results)
    print(report)

    if args.output:
        Path(args.output).write_text(report)
        print(f"\nReport saved to: {args.output}")


if __name__ == "__main__":
    main()
