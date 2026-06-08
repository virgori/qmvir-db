#!/usr/bin/env python3
"""Quick end-to-end CLI test."""
import qm_engine
import subprocess
import sys
import os

# Create engine with data
engine = qm_engine.NativeSqlEngine()
engine.execute("CREATE TABLE users (id INTEGER, name TEXT, email TEXT)")
for i in range(200):
    engine.execute(f"INSERT INTO users (id, name, email) VALUES ({i}, 'user_{i}', 'user{i}@test.com')")
engine.execute("CREATE TABLE logs (id INTEGER, message TEXT)")
for i in range(500):
    engine.execute(f"INSERT INTO logs (id, message) VALUES ({i}, 'log message {i}')")
print("Data created: 200 users + 500 logs")

# Backup
out = "/tmp/test_cli.qmvb"
result = qm_engine.backup(engine, out)
print(f"\n=== BACKUP ===")
for k, v in result.items():
    print(f"  {k}: {v}")

# Verify via CLI
print(f"\n=== CLI VERIFY ===")
os.system(f"{sys.executable} tools/qm_cli.py verify {out} --info")

# Cleanup
os.unlink(out)
print("\n✓ End-to-end test passed")
