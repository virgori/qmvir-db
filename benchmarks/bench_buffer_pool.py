#!/usr/bin/env python3
"""Buffer Pool benchmark — measures JOIN QPS with cache warm vs cold."""

import random
import time
import psycopg2

SEED = 42
PORT = 15433
ACCOUNTS = 5000
PRODUCTS = 500
ORDERS = 10000
WARMUP = 50
OPS = 500

def main():
    random.seed(SEED)

    # Start QM server
    from qm_engine import PostgresGateway
    gw = PostgresGateway(port=PORT, max_connections=200)
    gw.start_native()
    time.sleep(0.3)
    assert gw.is_running, "QM failed to start"

    conn = psycopg2.connect(host="127.0.0.1", port=PORT, user="admin", password="admin", dbname="qm")
    conn.autocommit = True
    cur = conn.cursor()

    # Setup tables
    sfx = f"_{int(time.time()) % 10000}"
    acc = f"bench_accounts{sfx}"
    prod = f"bench_products{sfx}"
    ords = f"bench_orders{sfx}"

    cur.execute(f"CREATE TABLE {acc} (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT)")
    cur.execute(f"CREATE TABLE {prod} (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, category TEXT)")
    cur.execute(f"CREATE TABLE {ords} (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total DOUBLE PRECISION)")

    cats = ["electronics", "books", "clothing", "food", "toys"]
    for i in range(1, ACCOUNTS + 1):
        cur.execute(f"INSERT INTO {acc} (id, balance, name) VALUES ({i}, {1000 + random.random()*1000:.2f}, 'user_{i}')")
    for i in range(1, PRODUCTS + 1):
        cur.execute(f"INSERT INTO {prod} (id, name, price, category) VALUES ({i}, 'product_{i}', {random.uniform(10,500):.2f}, '{random.choice(cats)}')")
    for i in range(1, ORDERS + 1):
        aid = random.randint(1, ACCOUNTS)
        pid = random.randint(1, PRODUCTS)
        qty = random.randint(1, 10)
        total = qty * random.uniform(10, 500)
        cur.execute(f"INSERT INTO {ords} (id, account_id, product_id, quantity, total) VALUES ({i}, {aid}, {pid}, {qty}, {total:.2f})")

    # Create index
    try:
        cur.execute(f"CREATE INDEX idx_{ords}_account ON {ords} (account_id)")
    except Exception:
        pass

    join_sql = (
        f"SELECT a.name, o.id, p.name, o.quantity, o.total "
        f"FROM {acc} a "
        f"JOIN {ords} o ON a.id = o.account_id "
        f"JOIN {prod} p ON o.product_id = p.id "
        f"WHERE a.id = %s"
    )

    rnd = random.Random(SEED + 1)

    # Warmup (also warms buffer pool cache)
    print(f"Warming up ({WARMUP} queries)...")
    for _ in range(WARMUP):
        cur.execute(join_sql, (rnd.randint(1, ACCOUNTS),))
        cur.fetchall()

    # Benchmark: cache-warm JOIN queries
    print(f"Benchmarking {OPS} JOIN queries (buffer pool warm)...")
    latencies = []
    t0 = time.perf_counter()
    for _ in range(OPS):
        s = time.perf_counter()
        cur.execute(join_sql, (rnd.randint(1, ACCOUNTS),))
        cur.fetchall()
        latencies.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    qps = OPS / elapsed
    avg_ms = sum(latencies) / len(latencies)
    latencies.sort()
    p50 = latencies[len(latencies)//2]
    p95 = latencies[int(len(latencies)*0.95)]
    p99 = latencies[int(len(latencies)*0.99)]

    print(f"\n=== Buffer Pool JOIN Benchmark ===")
    print(f"Dataset: {ACCOUNTS} accounts, {PRODUCTS} products, {ORDERS} orders")
    print(f"Queries: {OPS}")
    print(f"QPS:     {qps:,.0f}")
    print(f"Avg:     {avg_ms:.2f} ms")
    print(f"P50:     {p50:.2f} ms")
    print(f"P95:     {p95:.2f} ms")
    print(f"P99:     {p99:.2f} ms")
    print(f"Total:   {elapsed:.3f}s")

    cur.close()
    conn.close()
    gw.stop()
    print("\nDone.")


if __name__ == "__main__":
    main()
