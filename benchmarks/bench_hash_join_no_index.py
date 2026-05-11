#!/usr/bin/env python3
"""Benchmark full-scan hash JOIN (no B+Tree index) to isolate buffer pool impact."""

import random
import time
import psycopg2

SEED = 42
PORT = 15435
ACCOUNTS = 10000
PRODUCTS = 1000
ORDERS = 50000
OPS = 200


def bench(cur, sql, ops, label):
    rnd = random.Random(SEED + 3)
    lat = []
    t0 = time.perf_counter()
    for _ in range(ops):
        s = time.perf_counter()
        cur.execute(sql, (rnd.randint(1, ACCOUNTS),))
        cur.fetchall()
        lat.append((time.perf_counter() - s) * 1000)
    elapsed = time.perf_counter() - t0
    qps = ops / elapsed
    lat.sort()
    print(f"  [{label}] QPS={qps:,.0f}  Avg={sum(lat)/len(lat):.3f}ms  P50={lat[len(lat)//2]:.3f}ms  P99={lat[int(len(lat)*0.99)]:.3f}ms")
    return qps


def main():
    random.seed(SEED)
    from qm_engine import PostgresGateway

    gw = PostgresGateway(port=PORT, max_connections=200)
    gw.start_native()
    time.sleep(0.3)

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
    print(f"Loading {ACCOUNTS} accounts, {PRODUCTS} products, {ORDERS} orders...")
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

    # NO INDEX — forces HashJoinParallel path
    join_sql = (
        f"SELECT a.name, o.id, p.name, o.quantity, o.total "
        f"FROM {acc} a "
        f"JOIN {ords} o ON a.id = o.account_id "
        f"JOIN {prod} p ON o.product_id = p.id "
        f"WHERE a.id = %s"
    )

    print(f"\n=== Hash JOIN (no index) — Buffer Pool Impact ===")
    print(f"Dataset: {ACCOUNTS}a / {PRODUCTS}p / {ORDERS}o\n")

    qps1 = bench(cur, join_sql, OPS, "COLD-1")
    qps2 = bench(cur, join_sql, OPS, "WARM-2")
    qps3 = bench(cur, join_sql, OPS, "WARM-3")

    print(f"\nSpeedup WARM/COLD: {qps2/qps1:.2f}x")
    print(f"Sustained WARM:    {qps3:,.0f} QPS")

    cur.close()
    conn.close()
    gw.stop()


if __name__ == "__main__":
    main()
