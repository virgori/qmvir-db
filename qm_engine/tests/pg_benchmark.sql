-- ═══════════════════════════════════════════════════════════════
-- PostgreSQL Benchmark Script — Compare with QM Engine
-- 2026-04-11
-- ═══════════════════════════════════════════════════════════════

\timing on

-- ═══ 1. Setup ═══
CREATE TABLE bench_1m (
    id SERIAL PRIMARY KEY,
    status INTEGER,
    category INTEGER,
    value DOUBLE PRECISION,
    payload TEXT
);

-- Insert 1M rows
INSERT INTO bench_1m (status, category, value, payload)
SELECT
    (random() * 4)::int,
    (random() * 99)::int,
    random() * 100000,
    md5(random()::text)
FROM generate_series(1, 1000000);

-- Create BTree index
CREATE INDEX idx_status ON bench_1m (status);
CREATE INDEX idx_category ON bench_1m (category);

ANALYZE bench_1m;

-- ═══ 2. Seq scan vs Index scan ═══
\echo '=== SEQ SCAN (full table) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m;

\echo '=== INDEX SCAN (selective: status = 0, ~20%) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m WHERE status = 0;

\echo '=== INDEX SCAN (very selective: id < 1000, ~0.1%) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m WHERE id < 1000;

-- ═══ 3. Bitmap scan ═══
\echo '=== BITMAP SCAN (category = 50, ~1%) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m WHERE category = 50;

-- ═══ 4. Aggregates / Statistics ═══
\echo '=== COUNT DISTINCT (exact) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT COUNT(DISTINCT status) FROM bench_1m;

\echo '=== QUANTILE (percentile) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT
    percentile_cont(0.5) WITHIN GROUP (ORDER BY value) AS p50,
    percentile_cont(0.99) WITHIN GROUP (ORDER BY value) AS p99
FROM bench_1m;

-- ═══ 5. Full-text search ═══
CREATE TABLE bench_docs (
    id SERIAL PRIMARY KEY,
    body TEXT
);
INSERT INTO bench_docs (body)
SELECT 'the quick brown fox jumps over the lazy dog data query search index engine fast rust compile ' || 
       md5(i::text)
FROM generate_series(1, 10000) AS i;

CREATE INDEX idx_fts ON bench_docs USING gin(to_tsvector('english', body));
ANALYZE bench_docs;

\echo '=== FULL TEXT SEARCH (GIN index) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT)
SELECT id, ts_rank(to_tsvector('english', body), plainto_tsquery('english', 'quick fox search')) AS score
FROM bench_docs
WHERE to_tsvector('english', body) @@ plainto_tsquery('english', 'quick fox search')
ORDER BY score DESC LIMIT 10;

-- ═══ 6. Bloom filter (pg_trgm / btree comparison) ═══
\echo '=== BTREE point lookup (1M rows, PK) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m WHERE id = 500000;

-- ═══ 7. Join benchmark ═══
CREATE TABLE bench_small (
    id SERIAL PRIMARY KEY,
    value TEXT
);
INSERT INTO bench_small (value)
SELECT md5(i::text) FROM generate_series(1, 1000) AS i;
ANALYZE bench_small;

\echo '=== HASH JOIN (1M × 1K) ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT)
SELECT COUNT(*) FROM bench_1m b JOIN bench_small s ON b.category = s.id;

-- ═══ 8. Sort performance ═══
\echo '=== SORT 1M rows ==='
EXPLAIN (ANALYZE, COSTS, TIMING, FORMAT TEXT) SELECT * FROM bench_1m ORDER BY value LIMIT 100;

-- ═══ 9. Data integrity check ═══
\echo '=== DATA INTEGRITY ==='
SELECT
    COUNT(*) AS total_rows,
    COUNT(DISTINCT id) AS distinct_ids,
    MIN(id) AS min_id,
    MAX(id) AS max_id,
    (COUNT(*) = COUNT(DISTINCT id))::text AS "ids_unique",
    (MAX(id) - MIN(id) + 1 = COUNT(*))::text AS "ids_contiguous"
FROM bench_1m;

-- ═══ 10. Memory/Size stats ═══
\echo '=== TABLE AND INDEX SIZES ==='
SELECT
    pg_size_pretty(pg_relation_size('bench_1m')) AS "table_size",
    pg_size_pretty(pg_indexes_size('bench_1m')) AS "index_size",
    pg_size_pretty(pg_total_relation_size('bench_1m')) AS "total_size",
    pg_size_pretty(pg_relation_size('bench_docs')) AS "docs_table",
    pg_size_pretty(pg_indexes_size('bench_docs')) AS "docs_index";

\echo '=== DONE ==='
