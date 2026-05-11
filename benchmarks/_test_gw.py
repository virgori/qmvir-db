#!/usr/bin/env python3
"""Test QMvir gateway connection."""
import time
import sys

import qm_engine

PORT = 15435
print(f"Starting gateway on port {PORT}...", flush=True)
gw = qm_engine.PostgresGateway(port=PORT, max_connections=100)
gw.start_native()
time.sleep(1.0)
print(f"Running: {gw.is_running}", flush=True)

if not gw.is_running:
    print("FAILED: gateway not running", flush=True)
    sys.exit(1)

import psycopg2
try:
    conn = psycopg2.connect(host="127.0.0.1", port=PORT, user="admin", password="admin", dbname="qm")
    conn.autocommit = True
    cur = conn.cursor()
    print("Connected!", flush=True)

    cur.execute("CREATE TABLE test1 (id INTEGER PRIMARY KEY, balance DOUBLE PRECISION, name TEXT)")
    print("Table created", flush=True)

    for i in range(10):
        cur.execute(f"INSERT INTO test1 (id, balance, name) VALUES ({i}, {1000+i*100}, 'user_{i}')")
    print("10 rows inserted", flush=True)

    cur.execute("SELECT COUNT(*) FROM test1")
    row = cur.fetchone()
    print(f"COUNT: {row}", flush=True)

    cur.execute("SELECT * FROM test1 WHERE id = 5")
    rows = cur.fetchall()
    print(f"WHERE id=5: {rows}", flush=True)

    conn.close()
except Exception as e:
    print(f"Error: {e}", flush=True)
    import traceback
    traceback.print_exc()
finally:
    gw.stop()
    print("Done", flush=True)
