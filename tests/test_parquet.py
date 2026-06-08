#!/usr/bin/env python3
"""Test Parquet COPY FROM integration."""

import os
import sys
import time
import socket
import struct
import tempfile

def run_sql(query, port=55499):
    """Send a simple PG wire protocol query and return raw response."""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.settimeout(5.0)
    s.connect(('127.0.0.1', port))
    # Startup
    user = b'admin\0'
    db = b'test\0'
    startup = b'\x00\x03\x00\x00user\0' + user + b'database\0' + db + b'\0'
    s.sendall(struct.pack('!I', len(startup) + 4) + startup)
    resp = s.recv(4096)
    # Query
    q_bytes = query.encode() + b'\0'
    s.sendall(b'Q' + struct.pack('!I', len(q_bytes) + 4) + q_bytes)
    result = b''
    while True:
        try:
            chunk = s.recv(4096)
            if not chunk:
                break
            result += chunk
            if b'Z' in chunk:
                break
        except socket.timeout:
            break
    s.close()
    return result

def extract_data_row(resp):
    """Extract first DataRow value from PG wire response."""
    idx = resp.find(b'D')
    if idx < 0:
        return None
    # Skip 'D', read length (4 bytes), num_cols (2 bytes)
    length = struct.unpack('!I', resp[idx+1:idx+5])[0]
    num_cols = struct.unpack('!H', resp[idx+5:idx+7])[0]
    pos = idx + 7
    values = []
    for _ in range(num_cols):
        col_len = struct.unpack('!i', resp[pos:pos+4])[0]
        pos += 4
        if col_len < 0:
            values.append(None)
        else:
            values.append(resp[pos:pos+col_len].decode())
            pos += col_len
    return values


def main():
    # Create test Parquet file
    try:
        import pyarrow as pa
        import pyarrow.parquet as pq
    except ImportError:
        print("SKIP: pyarrow not installed")
        return

    parquet_path = '/tmp/test_qm_parquet.parquet'
    table = pa.table({
        'id': pa.array(range(1, 10001), type=pa.int64()),
        'name': pa.array([f'user_{i}' for i in range(1, 10001)], type=pa.string()),
        'balance': pa.array([float(i) * 1.5 for i in range(1, 10001)], type=pa.float64()),
        'score': pa.array([i % 100 for i in range(1, 10001)], type=pa.int64()),
    })
    pq.write_table(table, parquet_path, compression='zstd')
    print(f"Created {parquet_path}: {table.num_rows} rows, {os.path.getsize(parquet_path)} bytes")

    # Start QMvir engine
    import qm_engine
    data_dir = tempfile.mkdtemp(prefix='qm_parquet_test_')
    port = 55499
    gw = qm_engine.PostgresGateway('127.0.0.1', port)
    gw.start_native_persist(data_dir)
    time.sleep(0.5)

    try:
        # 1. COPY FROM Parquet
        print("\n--- Test 1: COPY FROM Parquet ---")
        resp = run_sql(f"COPY parquet_users FROM '{parquet_path}'", port)
        vals = extract_data_row(resp)
        print(f"  COPY result: {vals}")
        assert vals is not None, "COPY returned no data"
        assert 'COPY 10000' in vals[0], f"Expected COPY 10000, got {vals[0]}"
        print("  PASSED: 10,000 rows loaded from Parquet")

        # 2. Verify COUNT
        print("\n--- Test 2: COUNT(*) ---")
        resp = run_sql("SELECT COUNT(*) FROM parquet_users", port)
        vals = extract_data_row(resp)
        print(f"  COUNT: {vals}")
        assert vals is not None and vals[0] == '10000', f"Expected 10000, got {vals}"
        print("  PASSED: Row count matches")

        # 3. Verify SUM
        print("\n--- Test 3: SUM(balance) ---")
        resp = run_sql("SELECT SUM(balance) FROM parquet_users", port)
        vals = extract_data_row(resp)
        print(f"  SUM: {vals}")
        expected = sum(i * 1.5 for i in range(1, 10001))
        actual = float(vals[0])
        assert abs(actual - expected) < 1.0, f"Expected ~{expected}, got {actual}"
        print(f"  PASSED: SUM = {actual} (expected {expected})")

        # 4. Verify range scan using BETWEEN
        print("\n--- Test 4: Range scan (BETWEEN) ---")
        resp = run_sql("SELECT COUNT(*) FROM parquet_users WHERE id BETWEEN 500 AND 600", port)
        # COUNT is embedded in command tag
        cnt_resp = run_sql("SELECT SUM(balance) FROM parquet_users WHERE id BETWEEN 500 AND 600", port)
        vals = extract_data_row(cnt_resp)
        print(f"  SUM(balance) for id 500-600: {vals}")
        expected_range = sum(i * 1.5 for i in range(500, 601))
        actual_range = float(vals[0])
        assert abs(actual_range - expected_range) < 1.0, f"Expected ~{expected_range}, got {actual_range}"
        print(f"  PASSED: Range SUM = {actual_range}")

        # 5. Test COPY with FORMAT keyword
        print("\n--- Test 5: COPY WITH FORMAT PARQUET ---")
        resp = run_sql(f"COPY parquet_users2 FROM '{parquet_path}' (FORMAT PARQUET)", port)
        vals = extract_data_row(resp)
        print(f"  Result: {vals}")
        assert vals is not None and 'COPY 10000' in vals[0]
        print("  PASSED: FORMAT PARQUET keyword works")

        print("\n" + "=" * 50)
        print("ALL PARQUET INTEGRATION TESTS PASSED!")
        print("=" * 50)

    finally:
        # Cleanup
        os.remove(parquet_path)
        import shutil
        shutil.rmtree(data_dir, ignore_errors=True)


if __name__ == '__main__':
    main()
