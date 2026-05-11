#!/usr/bin/env python3
"""Buffer Pool A/B benchmark — measures cache-warm vs cache-cold JOIN QPS back-to-back."""

import random
import time
import psycopg2

SEED = 42
PORT = 15434
ACCOUNTS = 10000
PRODUCTS = 1000
ORDERS = 50000
WARMUP = 30
OPS = 500


def bench_join(cur, join_sql, ops, accounts_n, label):
    rnd = random.Random(SEED + 7)
    latencies = []
    t0 = time.perf_counter()
    for _ in range(ops):
        s = time.perf_counter()
        cur.execute(join_sql, (rnd.randint(1, accounts_n),))
        cur.fetchall()
        latencies.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0

    qps = ops / elapsed
    avg_ms = sum(latencies) / len(latencies)
    latencies.sort()
    p50 = latencies[len(latencies)//2]
    p99 = latencies[int(len(latencies)*0.99)]
    print(f"  [{label}] QPS={qps:,.0f}  Avg={avg_ms:.3f}ms  P50={p50:.3f}ms  P99={p99:.3f}ms")
    return qps


def main():
    random.seed(SEED)
    from qm_engine import PostgresGateway

    gw = PostgresGateway(port=PORT, max_connections=200)
    gw.start_native()
    time.sleep(0.3)
    assert gw.is_running

    conn = psycopg2.connect(host="127.0.0.1", port=PORT, user="admin", password="admin", dbname="qm")
    conn.autocommit = True
    cur = conn.cursor()

    sfx = f"_{int(time.time()) % 10000}"
    acc = f"bench_accounts{sfx}"
    prod = f"bench_products{sfx}"
    ords = f"bench_orders{sfx}"

    cur.execute(f"CREATE TABLE {acc} (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT)")
    cur.execute(f"CREATE TABLE {prod} (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, category TEXT)")
    cur.execute(f"CREATE TABLE {ords} (id INTEGER PRIMARY KEY, account_id INTEGER, product_id INTEGER, quantity INTEGER, total DOUBLE PRECISION)")

    cats = ["electronics", "books", "clothing", "food", "toys"]
    print(f"Inserting {ACCOUNTS} accounts, {PRODUCTS} products, {ORDERS} orders...")
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

    print(f"\n=== Buffer Pool A/B Benchmark ===")
    print(f"Dataset: {ACCOUNTS} accounts, {PRODUCTS} products, {ORDERS} orders\n")

    # Run 1: First-touch (cold cache — dimension hash + SoA built from scratch)
    print("Run 1 (cold cache — first time building hash tables + SoA):")
    qps_cold = bench_join(cur, join_sql, OPS, ACCOUNTS, "COLD")

    # Run 2: Warm cache — should be faster since dim hash + SoA are cached
    print("Run 2 (warm cache — reusing cached data):")
    qps_warm = bench_join(cur, join_sql, OPS, ACCOUNTS, "WARM")

    # Run 3: After mutation (invalidation) — insert 100 new orders
    print("Inserting 100 new orders to invalidate cache...")
    for i in range(ORDERS + 1, ORDERS + 101):
        aid = random.randint(1, ACCOUNTS)
        pid = random.randint(1, PRODUCTS)
        qty = random.randint(1, 10)
        total = qty * random.uniform(10, 500)
        cur.execute(f"INSERT INTO {ords} (id, account_id, product_id, quantity, total) VALUES ({i}, {aid}, {pid}, {qty}, {total:.2f})")

    print("Run 3 (post-mutation — cache invalidated, then re-cached):")
    qps_post = bench_join(cur, join_sql, OPS, ACCOUNTS, "POST-INVALIDATION")

    # Run 4: Warm again after rebuild
    print("Run 4 (warm again after rebuild):")
    qps_warm2 = bench_join(cur, join_sql, OPS, ACCOUNTS, "RE-WARM")

    print(f"\n=== Summary ===")
    print(f"Cold:              {qps_cold:>10,.0f} QPS")
    print(f"Warm (cached):     {qps_warm:>10,.0f} QPS")
    print(f"Post-invalidation: {qps_post:>10,.0f} QPS")
    print(f"Re-warm (cached):  {qps_warm2:>10,.0f} QPS")
    if qps_cold > 0:
        print(f"Speedup warm/cold: {qps_warm/qps_cold:.2f}x")

    cur.close()
    conn.close()
    gw.stop()


if __name__ == "__main__":
    main()
