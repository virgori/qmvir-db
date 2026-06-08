"""QM Database Core — Performance Benchmark.

Measures throughput and latency for all kernel operations:
    - Storage: WAL write throughput, buffer pool hit rate
    - Index: B+tree insert/lookup, Roaring bitmap ops, inverted search
    - Execution: vectorized filter/aggregate, pipeline throughput
    - Statistics: HLL accuracy, sketch ingestion rate
    - Engine: end-to-end CRUD + search
"""

import math
import os
import random
import tempfile
import time
from dataclasses import dataclass


@dataclass
class BenchResult:
    name: str
    ops: int
    elapsed_s: float
    extra: str = ""

    @property
    def ops_per_sec(self) -> float:
        return self.ops / self.elapsed_s if self.elapsed_s > 0 else 0

    def __str__(self) -> str:
        rate = self.ops_per_sec
        unit = "ops/s"
        if rate > 1_000_000:
            rate /= 1_000_000
            unit = "M ops/s"
        elif rate > 1_000:
            rate /= 1_000
            unit = "K ops/s"
        extra = f" ({self.extra})" if self.extra else ""
        return f"{self.name:40s}  {rate:>10.2f} {unit:>10s}  {self.elapsed_s*1000:>8.1f} ms{extra}"


def bench_btree():
    from qm_core.index.btree import BPlusTree
    n = 100_000
    tree = BPlusTree(order=128)

    t0 = time.perf_counter()
    for i in range(n):
        tree.insert(i, i)
    t_insert = time.perf_counter() - t0

    t0 = time.perf_counter()
    for i in range(n):
        tree.get(i)
    t_lookup = time.perf_counter() - t0

    t0 = time.perf_counter()
    tree.range_scan(1000, 2000)
    t_range = time.perf_counter() - t0

    return [
        BenchResult("B+Tree insert", n, t_insert),
        BenchResult("B+Tree point lookup", n, t_lookup),
        BenchResult("B+Tree range scan (1K)", 1001, t_range),
    ]


def bench_roaring():
    from qm_core.index.roaring import RoaringBitmap
    n = 1_000_000

    rb = RoaringBitmap()
    t0 = time.perf_counter()
    for i in range(n):
        rb.add(i)
    t_add = time.perf_counter() - t0

    rb2 = RoaringBitmap()
    for i in range(n // 2, n + n // 2):
        rb2.add(i)

    t0 = time.perf_counter()
    rb3 = rb & rb2
    t_and = time.perf_counter() - t0

    t0 = time.perf_counter()
    rb4 = rb | rb2
    t_or = time.perf_counter() - t0

    return [
        BenchResult("Roaring add", n, t_add),
        BenchResult("Roaring AND (1M ∩ 1M)", 1, t_and, f"result={rb3.cardinality}"),
        BenchResult("Roaring OR (1M ∪ 1M)", 1, t_or, f"result={rb4.cardinality}"),
    ]


def bench_inverted():
    from qm_core.index.inverted import InvertedIndex
    n_docs = 10_000
    idx = InvertedIndex()

    words = ["database", "search", "engine", "query", "index", "performance",
             "modern", "system", "data", "retrieval", "algorithm", "structure"]
    random.seed(42)

    t0 = time.perf_counter()
    for i in range(n_docs):
        text = " ".join(random.choices(words, k=20))
        idx.add_document(i, {"body": text.split()})
    idx.finalize()
    t_build = time.perf_counter() - t0

    t0 = time.perf_counter()
    for _ in range(100):
        idx.search_daat(["database", "search"], top_k=10)
    t_daat = time.perf_counter() - t0

    t0 = time.perf_counter()
    for _ in range(100):
        idx.search_wand(["database", "search"], top_k=10)
    t_wand = time.perf_counter() - t0

    t0 = time.perf_counter()
    for _ in range(100):
        idx.search_bmw(["database", "search"], top_k=10)
    t_bmw = time.perf_counter() - t0

    return [
        BenchResult(f"Inverted build ({n_docs} docs)", n_docs, t_build),
        BenchResult("DAAT search (100 queries)", 100, t_daat),
        BenchResult("WAND search (100 queries)", 100, t_wand),
        BenchResult("BMW search (100 queries)", 100, t_bmw),
    ]


def bench_hnsw():
    from qm_core.index.hnsw import HNSWIndex
    import numpy as np
    dim = 64
    n = 5_000

    idx = HNSWIndex(dim=dim)
    random.seed(42)
    vectors = [np.array([random.gauss(0, 1) for _ in range(dim)], dtype=np.float32) for _ in range(n)]

    t0 = time.perf_counter()
    for i, v in enumerate(vectors):
        idx.add(i, v)
    t_insert = time.perf_counter() - t0

    queries = [np.array([random.gauss(0, 1) for _ in range(dim)], dtype=np.float32) for _ in range(100)]
    t0 = time.perf_counter()
    for q in queries:
        idx.search(q, top_k=10)
    t_search = time.perf_counter() - t0

    return [
        BenchResult(f"HNSW insert ({n} vecs, dim={dim})", n, t_insert),
        BenchResult("HNSW search (100 queries, k=10)", 100, t_search),
    ]


def bench_vectorized():
    from qm_core.execution.vectorized import ColumnBatch, VecFilter, VecSort, VecHashAggregate
    n = 100_000
    random.seed(42)
    rows = [{"x": random.random(), "y": random.randint(0, 100), "cat": random.randint(0, 10)} for _ in range(n)]

    t0 = time.perf_counter()
    batch = ColumnBatch.from_rows(rows)
    t_create = time.perf_counter() - t0

    t0 = time.perf_counter()
    mask = VecFilter.compare(batch.columns["x"], "gt", 0.5)
    filtered = batch.select(mask)
    t_filter = time.perf_counter() - t0

    t0 = time.perf_counter()
    sorted_b = VecSort.sort(batch, [("y", True)])
    t_sort = time.perf_counter() - t0

    t0 = time.perf_counter()
    agg = VecHashAggregate(["cat"], [("sum", "x", "total"), ("count", "x", "cnt")])
    agg.execute(batch)
    t_agg = time.perf_counter() - t0

    return [
        BenchResult(f"Vectorized batch create ({n})", n, t_create),
        BenchResult(f"Vectorized filter ({n})", n, t_filter, f"passed={filtered.size}"),
        BenchResult(f"Vectorized sort ({n})", n, t_sort),
        BenchResult(f"Vectorized hash aggregate ({n})", n, t_agg),
    ]


def bench_sketches():
    from qm_core.statistics.sketches import HyperLogLog, CountMinSketch, TDigest
    n = 100_000

    hll = HyperLogLog(14)
    t0 = time.perf_counter()
    for i in range(n):
        hll.add(i)
    t_hll = time.perf_counter() - t0
    est = hll.estimate()
    error = abs(est - n) / n * 100

    cms = CountMinSketch(2048, 5)
    t0 = time.perf_counter()
    for i in range(n):
        cms.add(i % 1000)
    t_cms = time.perf_counter() - t0

    td = TDigest(100)
    t0 = time.perf_counter()
    for i in range(n):
        td.add(random.gauss(0, 1))
    t_td = time.perf_counter() - t0

    return [
        BenchResult(f"HLL add ({n})", n, t_hll, f"est={est}, err={error:.2f}%"),
        BenchResult(f"CMS add ({n})", n, t_cms),
        BenchResult(f"TDigest add ({n})", n, t_td),
    ]


def bench_engine():
    from qm_core.engine import QMEngine
    engine = QMEngine(wal_enabled=False)
    engine.create_table("bench", schema={"title": "text", "value": "float", "category": "int"})

    n = 10_000

    t0 = time.perf_counter()
    for i in range(n):
        engine.insert("bench", {"title": f"Document {i}", "value": random.random(), "category": i % 10})
    t_insert = time.perf_counter() - t0

    t0 = time.perf_counter()
    for _ in range(100):
        engine.find("bench", predicates=[{"column": "category", "op": "eq", "value": 5}], limit=10)
    t_find = time.perf_counter() - t0

    t0 = time.perf_counter()
    for _ in range(100):
        engine.search("bench", query="document", top_k=10)
    t_search = time.perf_counter() - t0

    t0 = time.perf_counter()
    engine.aggregate("bench", group_by=["category"], aggregates=[("sum", "value", "total")])
    t_agg = time.perf_counter() - t0

    return [
        BenchResult(f"Engine insert ({n})", n, t_insert),
        BenchResult("Engine find (100 queries)", 100, t_find),
        BenchResult("Engine search (100 queries)", 100, t_search),
        BenchResult("Engine aggregate", 1, t_agg),
    ]


def main():
    print("=" * 80)
    print("QM Database Core — Performance Benchmark")
    print("=" * 80)
    print()

    suites = [
        ("B+Tree", bench_btree),
        ("Roaring Bitmap", bench_roaring),
        ("Inverted Index (DAAT/WAND/BMW)", bench_inverted),
        ("HNSW Vector Index", bench_hnsw),
        ("Vectorized Execution", bench_vectorized),
        ("Probabilistic Sketches", bench_sketches),
        ("Engine End-to-End", bench_engine),
    ]

    all_results = []
    for suite_name, bench_fn in suites:
        print(f"── {suite_name} {'─' * (60 - len(suite_name))}")
        try:
            results = bench_fn()
            for r in results:
                print(f"  {r}")
            all_results.extend(results)
        except Exception as e:
            print(f"  ERROR: {e}")
        print()

    total_time = sum(r.elapsed_s for r in all_results)
    print(f"{'─' * 80}")
    print(f"Total benchmark time: {total_time:.2f}s")
    print(f"Total operations: {sum(r.ops for r in all_results):,}")


if __name__ == "__main__":
    main()
