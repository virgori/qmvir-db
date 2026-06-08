"""Comprehensive tests for QM database core.

Tests all kernel layers:
    - Storage: WAL, segments, buffer pool, MVCC, compaction
    - Index: B+tree, Roaring bitmap, inverted/BMW, HNSW/PQ, stats
    - Execution: parser, planner, vectorized, pipeline
    - Statistics: cost model, sketches (HLL, CMS, t-digest, bloom)
    - Optimizer: adaptive executor, rewrite rules
    - Learned: selectivity, cache, fusion, intent
    - Engine: top-level integration
"""

import math
import os
import random
import tempfile
import unittest

# ── Storage tests ───────────────────────────────────────────────────

class TestWAL(unittest.TestCase):
    def test_append_and_replay(self):
        from qm_core.storage.wal import WriteAheadLog, WALOp
        with tempfile.TemporaryDirectory() as td:
            wal = WriteAheadLog(td)
            wal.open()
            for i in range(100):
                wal.append(WALOp.INSERT, txn_id=i, table="t",
                           key=str(i), data={"key": f"val_{i}"})
            records = wal.replay()
            self.assertEqual(len(records), 100)
            self.assertEqual(records[0].data["key"], "val_0")
            self.assertEqual(records[99].txn_id, 99)

    def test_checkpoint(self):
        from qm_core.storage.wal import WriteAheadLog, WALOp
        with tempfile.TemporaryDirectory() as td:
            wal = WriteAheadLog(td)
            wal.open()
            for i in range(10):
                wal.append(WALOp.INSERT, txn_id=i, table="t",
                           key=str(i), data={"k": i})
            wal.checkpoint()
            # After checkpoint, WAL is clean


class TestBufferPool(unittest.TestCase):
    def test_pin_unpin(self):
        from qm_core.storage.buffer_pool import BufferPool
        # BufferPool uses fetch_page(segment_id, page_id) → bytes
        bp = BufferPool(capacity=4)
        data = bp.fetch_page(0, 1)
        self.assertIsInstance(data, bytes)
        bp.unpin(0, 1)
        # Fetch again should be a cache hit
        data2 = bp.fetch_page(0, 1)
        self.assertEqual(data, data2)
        bp.unpin(0, 1)

    def test_eviction(self):
        from qm_core.storage.buffer_pool import BufferPool
        bp = BufferPool(capacity=3)
        for i in range(10):
            bp.fetch_page(0, i)
            bp.unpin(0, i)
        # Should not crash, oldest pages get evicted
        stats = bp.stats
        self.assertGreater(stats["evictions"], 0)


class TestMVCC(unittest.TestCase):
    def test_snapshot_isolation(self):
        from qm_core.storage.mvcc import MVCCEngine
        mvcc = MVCCEngine()
        t1 = mvcc.begin()
        mvcc.insert(t1, "default", "k1", {"v": "v1"})
        mvcc.commit(t1)

        t2 = mvcc.begin()
        row = mvcc.read(t2, "default", "k1")
        self.assertEqual(row["v"], "v1")

        # Concurrent write
        t3 = mvcc.begin()
        mvcc.update(t3, "default", "k1", {"v": "v2"})
        # t2 should still see old value
        row2 = mvcc.read(t2, "default", "k1")
        self.assertEqual(row2["v"], "v1")
        mvcc.commit(t3)
        mvcc.commit(t2)

    def test_rollback(self):
        from qm_core.storage.mvcc import MVCCEngine
        mvcc = MVCCEngine()
        t1 = mvcc.begin()
        mvcc.insert(t1, "default", "k1", {"v": "original"})
        mvcc.commit(t1)

        t2 = mvcc.begin()
        mvcc.update(t2, "default", "k1", {"v": "modified"})
        mvcc.rollback(t2)

        t3 = mvcc.begin()
        row = mvcc.read(t3, "default", "k1")
        self.assertEqual(row["v"], "original")


class TestCompaction(unittest.TestCase):
    def test_leveled_plan(self):
        from qm_core.storage.compaction import CompactionEngine, CompactionPolicy, CompactionStrategy
        from qm_core.storage.segments import SegmentManager
        with tempfile.TemporaryDirectory() as td:
            seg_mgr = SegmentManager(td)
            engine = CompactionEngine(seg_mgr, CompactionPolicy(strategy=CompactionStrategy.LEVELED))
            # Just verify it doesn't crash with no segments
            plan = engine.plan()
            self.assertEqual(len(plan), 0)


# ── Index tests ─────────────────────────────────────────────────────

class TestBPlusTree(unittest.TestCase):
    def test_insert_and_get(self):
        from qm_core.index.btree import BPlusTree
        tree = BPlusTree(order=4)
        for i in range(100):
            tree.insert(i, f"val_{i}")
        for i in range(100):
            self.assertEqual(tree.get(i), f"val_{i}")

    def test_range_scan(self):
        from qm_core.index.btree import BPlusTree
        tree = BPlusTree(order=4)
        for i in range(50):
            tree.insert(i, i * 10)
        result = tree.range_scan(10, 20)
        self.assertEqual(len(result), 11)
        self.assertEqual(result[0], (10, 100))

    def test_bulk_load(self):
        from qm_core.index.btree import BPlusTree
        tree = BPlusTree(order=8)
        pairs = [(i, f"v{i}") for i in range(1000)]
        tree.bulk_load(pairs)
        for k, v in pairs:
            self.assertEqual(tree.get(k), v)

    def test_delete(self):
        from qm_core.index.btree import BPlusTree
        tree = BPlusTree(order=4)
        for i in range(20):
            tree.insert(i, i)
        tree.delete(10)
        self.assertIsNone(tree.get(10))
        self.assertEqual(tree.get(9), 9)
        self.assertEqual(tree.get(11), 11)


class TestRoaringBitmap(unittest.TestCase):
    def test_basic_ops(self):
        from qm_core.index.roaring import RoaringBitmap
        rb = RoaringBitmap()
        for i in range(1000):
            rb.add(i)
        self.assertTrue(rb.contains(500))
        self.assertFalse(rb.contains(1001))
        self.assertEqual(rb.cardinality, 1000)

    def test_set_operations(self):
        from qm_core.index.roaring import RoaringBitmap
        a = RoaringBitmap()
        b = RoaringBitmap()
        for i in range(100):
            a.add(i)
        for i in range(50, 150):
            b.add(i)
        # AND
        c = a & b
        self.assertEqual(c.cardinality, 50)
        # OR
        d = a | b
        self.assertEqual(d.cardinality, 150)

    def test_large_bitmap(self):
        from qm_core.index.roaring import RoaringBitmap
        rb = RoaringBitmap()
        # Force bitmap container (>4096 elements in same high16 group)
        for i in range(0, 10000):
            rb.add(i)
        self.assertEqual(rb.cardinality, 10000)
        self.assertTrue(rb.contains(5000))

    def test_serialization(self):
        from qm_core.index.roaring import RoaringBitmap
        rb = RoaringBitmap()
        for i in range(500):
            rb.add(i * 3)
        data = rb.serialize()
        rb2 = RoaringBitmap.deserialize(data)
        self.assertEqual(rb.cardinality, rb2.cardinality)
        for i in range(500):
            self.assertTrue(rb2.contains(i * 3))


class TestInvertedIndex(unittest.TestCase):
    def test_basic_search(self):
        from qm_core.index.inverted import InvertedIndex
        idx = InvertedIndex()
        idx.add_document(1, {"body": "the quick brown fox".split()})
        idx.add_document(2, {"body": "the lazy brown dog".split()})
        idx.add_document(3, {"body": "quick fox jumps over".split()})
        idx.finalize()

        results = idx.search_daat(["quick", "fox"], top_k=2)
        self.assertGreater(len(results), 0)
        # Doc 1 and 3 have both terms
        doc_ids = {r[0] for r in results}
        self.assertIn(1, doc_ids)

    def test_bmw_search(self):
        from qm_core.index.inverted import InvertedIndex
        idx = InvertedIndex()
        for i in range(100):
            idx.add_document(i, {"body": f"document number {i} about databases and search engines".split()})
        idx.finalize()

        results = idx.search_bmw(["databases", "search"], top_k=5)
        self.assertEqual(len(results), 5)
        # All scores should be positive
        for doc_id, score in results:
            self.assertGreater(score, 0)

    def test_wand_search(self):
        from qm_core.index.inverted import InvertedIndex
        idx = InvertedIndex()
        for i in range(50):
            idx.add_document(i, {"body": f"test document {i} with some words about topic {i % 5}".split()})
        idx.finalize()

        results = idx.search_wand(["document", "topic"], top_k=3)
        self.assertGreater(len(results), 0)


class TestHNSW(unittest.TestCase):
    def test_basic_search(self):
        from qm_core.index.hnsw import HNSWIndex, Metric
        import numpy as np
        idx = HNSWIndex(dim=4, metric=Metric.EUCLIDEAN)
        # Insert some vectors
        for i in range(50):
            vec = np.array([float(i), float(i) * 0.5, float(i) * 0.3, float(i) * 0.1], dtype=np.float32)
            idx.add(i, vec)

        # Search for nearest to [25, 12.5, 7.5, 2.5]
        results = idx.search(np.array([25.0, 12.5, 7.5, 2.5], dtype=np.float32), top_k=3)
        self.assertGreater(len(results), 0)
        # Doc 25 should be the closest
        doc_ids = [r.id for r in results]
        self.assertIn(25, doc_ids)

    def test_product_quantizer(self):
        from qm_core.index.hnsw import ProductQuantizer
        import numpy as np
        import random
        random.seed(42)

        dim = 8
        n_sub = 4
        pq = ProductQuantizer(dim=dim, n_subvectors=n_sub, n_bits=8)

        # Generate training data
        train_data = np.array([[random.gauss(0, 1) for _ in range(dim)] for _ in range(200)], dtype=np.float32)
        pq.train(train_data)

        # Encode and decode
        vec = np.array([random.gauss(0, 1) for _ in range(dim)], dtype=np.float32)
        codes = pq.encode(vec.reshape(1, -1))
        self.assertEqual(codes.shape, (1, n_sub))

        decoded = pq.decode(codes)
        self.assertEqual(decoded.shape[1], dim)


class TestStats(unittest.TestCase):
    def test_analyze_column(self):
        from qm_core.index.stats import StatsCollector
        sc = StatsCollector()
        values = list(range(1000))
        sc.analyze_column("test_table", "id", values)
        table_stats = sc.get_table_stats("test_table")
        self.assertIsNotNone(table_stats)
        col_stats = table_stats.columns["id"]
        self.assertEqual(col_stats.row_count, 1000)
        self.assertGreater(col_stats.distinct_count, 900)

    def test_selectivity_estimation(self):
        from qm_core.index.stats import StatsCollector
        sc = StatsCollector()
        values = list(range(100))
        sc.analyze_column("t", "x", values)
        ts = sc.get_table_stats("t")
        sel = ts.columns["x"].estimate_selectivity("eq", 50)
        self.assertGreater(sel, 0)
        self.assertLess(sel, 0.1)


# ── Execution tests ─────────────────────────────────────────────────

class TestParser(unittest.TestCase):
    def test_find_query(self):
        from qm_core.execution.parser import QueryParser
        p = QueryParser()
        ast = p.parse({
            "action": "find",
            "table": "users",
            "where": {"age": {"gt": 18}},
            "limit": 10,
        })
        self.assertEqual(ast.action, "find")
        self.assertEqual(ast.table, "users")
        self.assertEqual(len(ast.predicates), 1)

    def test_search_query(self):
        from qm_core.execution.parser import QueryParser
        p = QueryParser()
        ast = p.parse({
            "action": "search",
            "table": "docs",
            "query": "database systems",
            "top_k": 5,
        })
        self.assertEqual(ast.action, "search")
        self.assertEqual(ast.query_text, "database systems")


class TestPlanner(unittest.TestCase):
    def test_find_plan(self):
        from qm_core.execution.parser import QueryParser, QueryAST
        from qm_core.execution.planner import QueryPlanner
        planner = QueryPlanner()
        ast = QueryAST(action="find", table="docs")
        plan = planner.plan(ast)
        self.assertIsNotNone(plan)
        explained = plan.explain()
        self.assertIn("seq_scan", explained)

    def test_search_plan(self):
        from qm_core.execution.parser import QueryAST
        from qm_core.execution.planner import QueryPlanner
        planner = QueryPlanner()
        ast = QueryAST(action="search", table="docs", query_text="hello world", top_k=10)
        plan = planner.plan(ast)
        explained = plan.explain()
        self.assertIn("bmw_scan", explained)


class TestVectorized(unittest.TestCase):
    def test_column_batch(self):
        from qm_core.execution.vectorized import ColumnBatch
        rows = [{"a": 1, "b": "x"}, {"a": 2, "b": "y"}, {"a": 3, "b": "z"}]
        batch = ColumnBatch.from_rows(rows)
        self.assertEqual(batch.size, 3)
        back = batch.to_rows()
        self.assertEqual(len(back), 3)

    def test_vec_filter(self):
        from qm_core.execution.vectorized import ColumnBatch, VecFilter
        rows = [{"x": i} for i in range(100)]
        batch = ColumnBatch.from_rows(rows)
        mask = VecFilter.compare(batch.columns["x"], "gt", 50)
        filtered = batch.select(mask)
        self.assertEqual(filtered.size, 49)

    def test_vec_sort(self):
        from qm_core.execution.vectorized import ColumnBatch, VecSort
        rows = [{"x": random.randint(0, 100)} for _ in range(50)]
        batch = ColumnBatch.from_rows(rows)
        sorted_batch = VecSort.sort(batch, [("x", True)])
        result = sorted_batch.to_rows()
        for i in range(len(result) - 1):
            self.assertLessEqual(result[i]["x"], result[i + 1]["x"])

    def test_vec_hash_aggregate(self):
        from qm_core.execution.vectorized import ColumnBatch, VecHashAggregate
        rows = [{"category": i % 3, "amount": float(i)} for i in range(30)]
        batch = ColumnBatch.from_rows(rows)
        agg = VecHashAggregate(["category"], [("sum", "amount", "total"), ("count", "amount", "cnt")])
        result = agg.execute(batch)
        self.assertEqual(result.size, 3)


class TestPipeline(unittest.TestCase):
    def test_retrieval_pipeline(self):
        from qm_core.execution.pipeline import (
            RetrievalPipeline, PipelineContext, ScoredCandidate,
            CandidateGenStage, LightweightScoringStage,
        )

        def gen(ctx):
            return [ScoredCandidate(doc_id=i, score=1.0 / (i + 1)) for i in range(100)]

        def scorer(candidates, ctx):
            for c in candidates:
                c.score *= 2.0
            return candidates

        pipe = RetrievalPipeline()
        pipe.add_stage(CandidateGenStage(gen))
        pipe.add_stage(LightweightScoringStage(scorer))

        ctx = PipelineContext(query="test", top_k=5)
        results = pipe.execute(ctx)
        self.assertGreater(len(results), 0)
        # Should be sorted by score
        for i in range(len(results) - 1):
            self.assertGreaterEqual(results[i].score, results[i + 1].score)


# ── Statistics tests ────────────────────────────────────────────────

class TestCostModel(unittest.TestCase):
    def test_seq_scan(self):
        from qm_core.statistics.cost_model import CostModelV2, TableCatalog, TableMeta
        cat = TableCatalog()
        cat.register("t", TableMeta(row_count=10000, page_count=125))
        model = CostModelV2(catalog=cat)
        est = model.seq_scan("t")
        self.assertGreater(est.total_cost, 0)
        self.assertEqual(est.estimated_rows, 10000)

    def test_index_scan_cheaper_for_low_selectivity(self):
        from qm_core.statistics.cost_model import CostModelV2, TableCatalog, TableMeta, IndexMeta
        cat = TableCatalog()
        cat.register("t", TableMeta(row_count=100000, page_count=1250,
                                     indexes={"idx": IndexMeta(height=3)}))
        model = CostModelV2(catalog=cat)
        seq = model.seq_scan("t")
        idx = model.index_scan("t", "idx", selectivity=0.001)
        self.assertLess(idx.total_cost, seq.total_cost)


class TestHyperLogLog(unittest.TestCase):
    def test_cardinality(self):
        from qm_core.statistics.sketches import HyperLogLog
        hll = HyperLogLog(precision=14)
        n = 10000
        for i in range(n):
            hll.add(i)
        est = hll.estimate()
        # Should be within ~5% for p=14
        self.assertAlmostEqual(est, n, delta=n * 0.1)

    def test_merge(self):
        from qm_core.statistics.sketches import HyperLogLog
        h1 = HyperLogLog(14)
        h2 = HyperLogLog(14)
        for i in range(5000):
            h1.add(i)
        for i in range(3000, 8000):
            h2.add(i)
        merged = h1.merge(h2)
        est = merged.estimate()
        self.assertAlmostEqual(est, 8000, delta=800)


class TestCountMinSketch(unittest.TestCase):
    def test_frequency(self):
        from qm_core.statistics.sketches import CountMinSketch
        cms = CountMinSketch(width=2048, depth=5)
        for _ in range(100):
            cms.add("hello")
        for _ in range(50):
            cms.add("world")
        self.assertGreaterEqual(cms.estimate("hello"), 100)
        self.assertGreaterEqual(cms.estimate("world"), 50)
        # Unknown items should have ~0 count
        self.assertLess(cms.estimate("unknown_xyz"), 5)


class TestTDigest(unittest.TestCase):
    def test_median(self):
        from qm_core.statistics.sketches import TDigest
        td = TDigest(compression=100)
        for i in range(1000):
            td.add(float(i))
        median = td.quantile(0.5)
        self.assertAlmostEqual(median, 500, delta=50)

    def test_percentiles(self):
        from qm_core.statistics.sketches import TDigest
        td = TDigest(compression=200)
        random.seed(42)
        values = [random.gauss(100, 15) for _ in range(10000)]
        for v in values:
            td.add(v)
        p50 = td.percentile(50)
        self.assertAlmostEqual(p50, 100, delta=5)


class TestBloomFilter(unittest.TestCase):
    def test_membership(self):
        from qm_core.statistics.sketches import BloomFilter
        bf = BloomFilter(expected_items=1000, fp_rate=0.01)
        for i in range(1000):
            bf.add(f"item_{i}")
        # All inserted items should be found
        for i in range(1000):
            self.assertIn(f"item_{i}", bf)
        # False positive rate should be low
        fp = sum(1 for i in range(10000, 11000) if f"item_{i}" in bf)
        self.assertLess(fp, 30)  # ~1-3% expected


# ── Optimizer tests ─────────────────────────────────────────────────

class TestAdaptive(unittest.TestCase):
    def test_cardinality_fence(self):
        from qm_core.optimizer.adaptive import CardinalityFence
        fence = CardinalityFence(threshold=3.0, min_rows=10)
        self.assertFalse(fence.should_replan(100, 100))
        self.assertTrue(fence.should_replan(100, 500))
        self.assertFalse(fence.should_replan(100, 5))  # Too few rows

    def test_plan_history(self):
        from qm_core.optimizer.adaptive import PlanHistory, PlanRecord
        history = PlanHistory()
        history.record(PlanRecord(
            query_hash="abc", plan_type="SeqScan",
            estimated_cost=100, actual_time_ms=50,
            actual_rows=1000, estimated_rows=1000,
        ))
        self.assertEqual(history.get_best_plan("abc"), "SeqScan")


# ── Learned tests ───────────────────────────────────────────────────

class TestLearnedSelectivity(unittest.TestCase):
    def test_correction_converges(self):
        from qm_core.learned.assistants import LearnedSelectivity, SelectivitySample
        ls = LearnedSelectivity(learning_rate=0.2, min_samples=3)
        # Planner always estimates 0.1, actual is 0.5
        for _ in range(20):
            ls.feedback(SelectivitySample(
                table="t", column="c", operator="eq",
                value_hash=42, estimated=0.1, actual=0.5,
            ))
        corrected = ls.correct("t", "c", "eq", 0.1)
        # Should be much closer to 0.5 than the original 0.1
        self.assertGreater(corrected, 0.3)


class TestIntentClassifier(unittest.TestCase):
    def test_keyword(self):
        from qm_core.learned.assistants import QueryIntentClassifier
        ic = QueryIntentClassifier()
        self.assertEqual(ic.classify("python error"), "keyword")

    def test_semantic(self):
        from qm_core.learned.assistants import QueryIntentClassifier
        ic = QueryIntentClassifier()
        self.assertEqual(ic.classify("what is the difference between SQL and NoSQL databases"), "semantic")

    def test_analytical(self):
        from qm_core.learned.assistants import QueryIntentClassifier
        ic = QueryIntentClassifier()
        self.assertEqual(ic.classify("count users by country"), "analytical")


# ── Engine integration tests ────────────────────────────────────────

class TestEngine(unittest.TestCase):
    def setUp(self):
        from qm_core.engine import QMEngine
        self.engine = QMEngine(wal_enabled=False)
        self.engine.create_table("docs", schema={"title": "text", "body": "text", "views": "int"})

    def test_insert_and_find(self):
        self.engine.insert("docs", {"title": "Hello", "body": "World", "views": 42})
        self.engine.insert("docs", {"title": "Test", "body": "Document", "views": 10})
        results = self.engine.find("docs")
        self.assertEqual(len(results), 2)

    def test_find_with_predicate(self):
        for i in range(20):
            self.engine.insert("docs", {"title": f"Doc {i}", "body": f"Body {i}", "views": i * 10})
        results = self.engine.find("docs", predicates=[{"column": "views", "op": "gt", "value": 100}])
        self.assertEqual(len(results), 9)  # views: 110, 120, ..., 190

    def test_search(self):
        self.engine.insert("docs", {"title": "Database Systems", "body": "Introduction to modern databases", "views": 100})
        self.engine.insert("docs", {"title": "Search Engines", "body": "Building search engine from scratch", "views": 50})
        results = self.engine.search("docs", query="database", top_k=5)
        self.assertGreater(len(results), 0)

    def test_aggregate(self):
        for i in range(30):
            self.engine.insert("docs", {"title": f"Doc {i}", "body": "", "views": i % 5 * 10})
        results = self.engine.aggregate(
            "docs",
            aggregates=[("count", "title", "cnt"), ("sum", "views", "total_views")],
        )
        self.assertEqual(len(results), 1)
        self.assertEqual(results[0]["cnt"], 30)

    def test_explain(self):
        result = self.engine.explain({
            "action": "find",
            "table": "docs",
            "predicates": [{"column": "views", "op": "gt", "value": 50}],
        })
        self.assertIn("scan", result)

    def test_execute_dsl(self):
        self.engine.insert("docs", {"title": "Test", "body": "Body", "views": 42})
        results = self.engine.execute({
            "action": "find",
            "table": "docs",
            "limit": 5,
        })
        self.assertEqual(len(results), 1)

    def test_update_and_delete(self):
        doc_id = self.engine.insert("docs", {"title": "Original", "body": "Text", "views": 10})
        self.engine.update("docs", doc_id, {"title": "Updated"})
        results = self.engine.find("docs", predicates=[{"column": "title", "op": "eq", "value": "Updated"}])
        self.assertEqual(len(results), 1)

        self.engine.delete("docs", doc_id)
        results = self.engine.find("docs")
        self.assertEqual(len(results), 0)

    def test_analyze(self):
        for i in range(50):
            self.engine.insert("docs", {"title": f"Doc {i}", "body": f"Body {i}", "views": i})
        stats = self.engine.analyze("docs")
        self.assertEqual(stats["row_count"], 50)

    def test_engine_stats(self):
        stats = self.engine.stats()
        self.assertEqual(stats["version"], "1.0.0")
        self.assertIn("docs", stats["tables"])

    def test_create_btree_index(self):
        for i in range(100):
            self.engine.insert("docs", {"title": f"D{i}", "body": "", "views": i})
        self.engine.create_index("docs", "views_idx", columns=["views"], index_type="btree")
        # Should still work after index creation
        results = self.engine.find("docs", predicates=[{"column": "views", "op": "eq", "value": 50}])
        self.assertEqual(len(results), 1)

    def test_create_inverted_index(self):
        for i in range(20):
            self.engine.insert("docs", {"title": f"Document about topic {i % 3}", "body": f"Content {i}", "views": i})
        self.engine.create_index("docs", "text_idx", columns=["title", "body"], index_type="inverted")
        results = self.engine.search("docs", query="topic", top_k=5)
        self.assertGreater(len(results), 0)


if __name__ == "__main__":
    unittest.main()
