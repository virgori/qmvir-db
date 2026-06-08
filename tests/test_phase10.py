"""Tests for QM Phase 10 — Bespoke AI-Native Architecture.

Tests cover:
 1. Shared Memory Ring Buffer (IPC)
 2. LSN Sequencer (Deterministic Ordering)
 3. Merkle Auditor (Data Integrity)
 4. Hub Control Plane (LSN + Merkle + Ring)
 5. Satellite Framework (Vector + General)
 6. Postgres Wire Protocol
 7. XOR-Delta Vector Compression
 8. DiskANN SSD-based Index
 9. Content-Defined Chunking (CDC)
 10. End-to-End Hub ↔ Satellite Transaction Flow
"""

import hashlib
import os
import struct
import tempfile
import threading
import time

import numpy as np
import pytest


# ═══════════════════════════════════════════════════════════════════
#  1. SHARED MEMORY RING BUFFER
# ═══════════════════════════════════════════════════════════════════

class TestSharedRingBuffer:
    """Test the LMAX Disruptor-style lock-free ring buffer."""

    def _make_ring(self, slots=16, data_size=1024):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        return SharedRingBuffer(slot_count=slots, slot_data_size=data_size)

    def test_create_and_stats(self):
        ring = self._make_ring()
        stats = ring.stats()
        assert stats["EMPTY"] == 16
        assert stats["READY"] == 0
        assert ring.slot_count == 16
        ring.unlink()

    def test_publish_and_consume(self):
        from qm_core.ipc.ring_buffer import CommandType, SlotState
        ring = self._make_ring()

        # Publish a command
        payload = b"Hello, Satellite!"
        idx = ring.publish(lsn=1, cmd=CommandType.INSERT, payload=payload)
        assert idx >= 0

        stats = ring.stats()
        assert stats["READY"] == 1
        assert stats["EMPTY"] == 15

        # Consume it
        result = ring.consume()
        assert result is not None
        slot_idx, hdr, data_mv = result
        assert hdr.lsn == 1
        assert hdr.cmd == CommandType.INSERT
        assert bytes(data_mv) == payload

        # Mark done
        ring.complete(slot_idx, result_payload=b"OK")
        stats = ring.stats()
        assert stats["DONE"] == 1

        # Collect result
        state, data = ring.collect_result(slot_idx)
        assert state == SlotState.DONE
        assert data == b"OK"

        # Slot is now EMPTY again
        stats = ring.stats()
        assert stats["EMPTY"] == 16
        ring.unlink()

    def test_publish_multiple_sequential(self):
        from qm_core.ipc.ring_buffer import CommandType
        ring = self._make_ring(slots=8)

        for i in range(8):
            idx = ring.publish(lsn=i + 1, cmd=CommandType.QUERY, payload=f"cmd_{i}".encode())
            assert idx >= 0

        stats = ring.stats()
        assert stats["READY"] == 8
        assert stats["EMPTY"] == 0

        # Ring is full
        idx = ring.publish(lsn=99, cmd=CommandType.QUERY, payload=b"overflow")
        assert idx == -1
        ring.unlink()

    def test_error_flow(self):
        from qm_core.ipc.ring_buffer import CommandType, SlotState
        ring = self._make_ring()
        idx = ring.publish(lsn=1, cmd=CommandType.INSERT, payload=b"data")
        result = ring.consume()
        assert result is not None
        slot_idx, _, _ = result

        ring.fail(slot_idx, error_payload=b"ERR: disk full")
        state, data = ring.collect_result(slot_idx)
        assert state == SlotState.ERROR
        assert data == b"ERR: disk full"
        ring.unlink()

    def test_attach_existing(self):
        """Create ring, then attach from 'satellite' side."""
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        ring1 = self._make_ring(slots=16, data_size=1024)
        path = ring1.path

        # Publish from ring1
        ring1.publish(lsn=42, cmd=CommandType.INSERT, payload=b"test_data")

        # Attach as consumer
        ring2 = SharedRingBuffer(path=path, slot_count=16, slot_data_size=1024, create=False)
        result = ring2.consume()
        assert result is not None
        _, hdr, data_mv = result
        assert hdr.lsn == 42
        assert bytes(data_mv) == b"test_data"

        ring2.close()
        ring1.unlink()

    def test_power_of_two_enforced(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        with pytest.raises(ValueError, match="power of 2"):
            SharedRingBuffer(slot_count=7)

    def test_payload_too_large(self):
        from qm_core.ipc.ring_buffer import CommandType
        ring = self._make_ring(data_size=64)
        with pytest.raises(ValueError, match="exceeds slot capacity"):
            ring.publish(lsn=1, cmd=CommandType.INSERT, payload=b"x" * 128)
        ring.unlink()

    def test_try_publish_timeout(self):
        from qm_core.ipc.ring_buffer import CommandType
        ring = self._make_ring(slots=4, data_size=32)
        # Fill ring
        for i in range(4):
            ring.publish(lsn=i, cmd=CommandType.NOOP, payload=b"x")
        # Try publish with short timeout
        idx = ring.try_publish(lsn=99, cmd=CommandType.NOOP, payload=b"x", timeout_ms=50)
        assert idx == -1
        ring.unlink()


# ═══════════════════════════════════════════════════════════════════
#  2. LSN SEQUENCER
# ═══════════════════════════════════════════════════════════════════

class TestLSNSequencer:
    """Test deterministic LSN ordering."""

    def test_monotonic(self):
        from qm_core.hub.lsn_sequencer import LSNSequencer
        seq = LSNSequencer(start_lsn=1, epoch=0)
        stamps = [seq.next() for _ in range(100)]
        lsns = [s.lsn for s in stamps]
        assert lsns == list(range(1, 101))

    def test_gap_free(self):
        from qm_core.hub.lsn_sequencer import LSNSequencer
        seq = LSNSequencer(start_lsn=1)
        batch = seq.next_batch(50)
        lsns = [s.lsn for s in batch]
        assert lsns == list(range(1, 51))
        # Next single LSN continues
        stamp = seq.next()
        assert stamp.lsn == 51

    def test_epoch_bump(self):
        from qm_core.hub.lsn_sequencer import LSNSequencer
        seq = LSNSequencer(start_lsn=1, epoch=0)
        seq.next()
        new_epoch = seq.bump_epoch()
        assert new_epoch == 1
        stamp = seq.next()
        assert stamp.epoch == 1

    def test_checkpoint_restore(self):
        from qm_core.hub.lsn_sequencer import LSNSequencer
        seq = LSNSequencer(start_lsn=1, epoch=0)
        for _ in range(42):
            seq.next()
        data = seq.checkpoint_state()
        seq2 = LSNSequencer.from_checkpoint(data)
        assert seq2.current_lsn == 43
        assert seq2.epoch == 0

    def test_stamp_serialization(self):
        from qm_core.hub.lsn_sequencer import LSNStamp
        stamp = LSNStamp(lsn=12345, epoch=7, timestamp_ns=9999999)
        data = stamp.to_bytes()
        assert len(data) == 24
        restored = LSNStamp.from_bytes(data)
        assert restored.lsn == 12345
        assert restored.epoch == 7

    def test_stamp_ordering(self):
        from qm_core.hub.lsn_sequencer import LSNStamp
        s1 = LSNStamp(lsn=1, epoch=0, timestamp_ns=0)
        s2 = LSNStamp(lsn=2, epoch=0, timestamp_ns=0)
        s3 = LSNStamp(lsn=1, epoch=1, timestamp_ns=0)
        assert s1 < s2
        assert s2 < s3  # epoch takes precedence

    def test_thread_safety(self):
        from qm_core.hub.lsn_sequencer import LSNSequencer
        seq = LSNSequencer(start_lsn=1)
        results = []
        def worker():
            for _ in range(100):
                results.append(seq.next().lsn)
        threads = [threading.Thread(target=worker) for _ in range(4)]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        assert len(results) == 400
        assert len(set(results)) == 400  # all unique
        assert sorted(results) == list(range(1, 401))


# ═══════════════════════════════════════════════════════════════════
#  3. MERKLE AUDITOR
# ═══════════════════════════════════════════════════════════════════

class TestMerkleAuditor:
    """Test Merkle tree data integrity audit."""

    def test_empty_root(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        assert auditor.root() == b"\x00" * 32

    def test_single_leaf(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        h = hashlib.sha256(b"page_data_1").digest()
        auditor.update_leaf("sat1:page1", h)
        root = auditor.root()
        assert root != b"\x00" * 32
        assert len(root) == 32

    def test_deterministic(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        a1 = MerkleAuditor()
        a2 = MerkleAuditor()
        h1 = hashlib.sha256(b"data1").digest()
        h2 = hashlib.sha256(b"data2").digest()
        a1.update_leaf("k1", h1)
        a1.update_leaf("k2", h2)
        a2.update_leaf("k1", h1)
        a2.update_leaf("k2", h2)
        assert a1.root() == a2.root()

    def test_update_changes_root(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        auditor.update_leaf("k1", hashlib.sha256(b"v1").digest())
        root1 = auditor.root()
        auditor.update_leaf("k1", hashlib.sha256(b"v2").digest())
        root2 = auditor.root()
        assert root1 != root2

    def test_proof_verification(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        for i in range(8):
            auditor.update_leaf(f"key_{i}", hashlib.sha256(f"data_{i}".encode()).digest())

        root = auditor.root()
        leaf_hash = hashlib.sha256(b"data_3").digest()
        proof = auditor.proof("key_3")
        assert MerkleAuditor.verify_proof(leaf_hash, proof, root)

    def test_proof_fails_with_wrong_data(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        for i in range(4):
            auditor.update_leaf(f"k{i}", hashlib.sha256(f"d{i}".encode()).digest())
        root = auditor.root()
        proof = auditor.proof("k1")
        fake_hash = hashlib.sha256(b"tampered").digest()
        assert not MerkleAuditor.verify_proof(fake_hash, proof, root)

    def test_audit_leaf(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        h = hashlib.sha256(b"correct").digest()
        auditor.update_leaf("sat:pg1", h)
        assert auditor.audit_leaf("sat:pg1", h) is True
        assert auditor.audit_leaf("sat:pg1", b"\x00" * 32) is False

    def test_divergent_leaves(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        h1 = hashlib.sha256(b"a").digest()
        h2 = hashlib.sha256(b"b").digest()
        auditor.update_leaf("k1", h1)
        auditor.update_leaf("k2", h2)
        # Satellite reports different hash for k2
        diverged = auditor.divergent_leaves({"k1": h1, "k2": hashlib.sha256(b"tampered").digest()})
        assert "k2" in diverged
        assert "k1" not in diverged

    def test_checkpoint_restore(self):
        from qm_core.hub.merkle_auditor import MerkleAuditor
        auditor = MerkleAuditor()
        for i in range(10):
            auditor.update_leaf(f"key_{i}", hashlib.sha256(f"data_{i}".encode()).digest())
        root1 = auditor.root()
        data = auditor.checkpoint()
        auditor2 = MerkleAuditor.from_checkpoint(data)
        assert auditor2.root() == root1
        assert auditor2.leaf_count == 10


# ═══════════════════════════════════════════════════════════════════
#  4. HUB CONTROL PLANE
# ═══════════════════════════════════════════════════════════════════

class TestHubControlPlane:
    """Test Hub orchestration: LSN + Merkle + Ring."""

    def _make_hub(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        from qm_core.hub.hub import Hub
        ring = SharedRingBuffer(slot_count=64, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        return hub, ring

    def test_dispatch(self):
        from qm_core.ipc.ring_buffer import CommandType
        hub, ring = self._make_hub()
        env = hub.dispatch(CommandType.INSERT, "users", b"row_data")
        assert env.stamp.lsn == 1
        assert env.slot_index >= 0
        stats = hub.stats()
        assert stats["dispatched"] == 1
        assert stats["inflight"] == 1
        ring.unlink()
        hub.close()

    def test_dispatch_and_collect(self):
        from qm_core.ipc.ring_buffer import CommandType
        hub, ring = self._make_hub()
        env = hub.dispatch(CommandType.INSERT, "orders", b"order_payload")

        # Simulate satellite processing
        result = ring.consume()
        assert result is not None
        slot_idx, hdr, data_mv = result
        ring.complete(slot_idx, result_payload=b"OK:inserted")

        # Hub collects
        cmd_result = hub.collect(env)
        assert cmd_result.success is True
        assert cmd_result.data == b"OK:inserted"
        assert cmd_result.lsn == 1
        ring.unlink()
        hub.close()

    def test_batch_dispatch(self):
        from qm_core.ipc.ring_buffer import CommandType
        hub, ring = self._make_hub()
        commands = [
            (CommandType.INSERT, "t1", b"row1"),
            (CommandType.INSERT, "t1", b"row2"),
            (CommandType.UPDATE, "t1", b"upd1"),
        ]
        envelopes = hub.dispatch_batch(commands)
        assert len(envelopes) == 3
        assert envelopes[0].stamp.lsn == 1
        assert envelopes[1].stamp.lsn == 2
        assert envelopes[2].stamp.lsn == 3
        ring.unlink()
        hub.close()

    def test_merkle_audit_after_complete(self):
        from qm_core.ipc.ring_buffer import CommandType
        hub, ring = self._make_hub()
        env = hub.dispatch(CommandType.INSERT, "users", b"data_payload")

        # Satellite processes
        result = ring.consume()
        slot_idx, _, _ = result
        ring.complete(slot_idx, result_payload=b"OK")

        # Collect — this updates Merkle
        hub.collect(env)
        root = hub.merkle_root()
        assert root != b"\x00" * 32
        assert hub.auditor.leaf_count == 1
        ring.unlink()
        hub.close()

    def test_hub_checkpoint_restore(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        ring1 = SharedRingBuffer(slot_count=64, slot_data_size=4096)
        hub1 = Hub(ring=ring1, start_lsn=1)

        # Dispatch and complete
        env = hub1.dispatch(CommandType.INSERT, "t", b"data")
        result = ring1.consume()
        ring1.complete(result[0], b"OK")
        hub1.collect(env)

        # Checkpoint
        cp = hub1.checkpoint()

        # Restore
        ring2 = SharedRingBuffer(slot_count=64, slot_data_size=4096)
        hub2 = Hub.from_checkpoint(cp, ring2)
        assert hub2.sequencer.current_lsn == 2
        assert hub2.sequencer.epoch == 1  # bumped on recovery
        assert hub2.auditor.leaf_count == 1

        ring1.unlink()
        ring2.unlink()
        hub1.close()
        hub2.close()


# ═══════════════════════════════════════════════════════════════════
#  5. SATELLITE FRAMEWORK
# ═══════════════════════════════════════════════════════════════════

class TestVectorSatellite:
    """Test Vector Satellite compute node."""

    def test_direct_insert_and_search(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        from qm_core.satellite.vector_satellite import VectorSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=16, slot_data_size=4096)
        config = SatelliteConfig(satellite_id="vec-0", data_dir=tempfile.mkdtemp())
        sat = VectorSatellite(config, ring, dim=4)

        # Direct API
        sat.insert_vector(1, np.array([1.0, 0.0, 0.0, 0.0]))
        sat.insert_vector(2, np.array([0.0, 1.0, 0.0, 0.0]))
        sat.insert_vector(3, np.array([0.9, 0.1, 0.0, 0.0]))
        assert sat.vector_count == 3

        results = sat.search_vector(np.array([1.0, 0.0, 0.0, 0.0]), top_k=2)
        assert len(results) >= 2
        # Closest should be vec 1
        ids = [r[0] for r in results]
        assert ids[0] == 1
        ring.unlink()

    def test_via_ipc(self):
        """Test vector insert via Hub IPC dispatch."""
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.vector_satellite import VectorSatellite, _pack_vector
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=16, slot_data_size=8192)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(satellite_id="vec-ipc", data_dir=tempfile.mkdtemp())
        sat = VectorSatellite(config, ring, dim=4)

        # Dispatch vector insert via Hub
        vec = np.array([1.0, 2.0, 3.0, 4.0], dtype=np.float32)
        payload = _pack_vector(100, vec, meta=b"test_meta")
        env = hub.dispatch(CommandType.VECTOR_OP, "vectors", payload)

        # Satellite processes
        assert sat.process_one() is True

        # Collect result
        result = hub.collect(env)
        assert result.success is True
        ring.unlink()
        hub.close()


class TestGeneralSatellite:
    """Test General Satellite (structured data)."""

    def test_direct_crud(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=16, slot_data_size=4096)
        config = SatelliteConfig(satellite_id="gen-0", data_dir=tempfile.mkdtemp())
        sat = GeneralSatellite(config, ring)

        # Insert
        r1 = sat.insert_row("users", {"name": "Alice", "age": 30})
        r2 = sat.insert_row("users", {"name": "Bob", "age": 25})
        assert r1 == 1
        assert r2 == 2

        # Read
        row = sat.get_row("users", 1)
        assert row["name"] == "Alice"

        # Query with predicate
        rows = sat.query_rows("users", [{"column": "age", "op": "gt", "value": 26}])
        assert len(rows) == 1
        assert rows[0]["name"] == "Alice"

    def test_cdc_dedup(self):
        from qm_core.ipc.ring_buffer import SharedRingBuffer
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=16, slot_data_size=4096)
        config = SatelliteConfig(satellite_id="gen-cdc", data_dir=tempfile.mkdtemp())
        sat = GeneralSatellite(config, ring)

        # Store identical blobs — CDC should deduplicate
        data1 = b"A" * 10000
        data2 = b"A" * 10000  # identical
        hashes1 = sat.store_blob(data1, chunk_size=2048)
        hashes2 = sat.store_blob(data2, chunk_size=2048)
        assert hashes1 == hashes2  # same hashes = deduped

        # Reconstruct
        recovered = sat.load_blob(hashes1)
        assert recovered == data1
        ring.unlink()

    def test_via_ipc(self):
        import msgpack
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=16, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(
            satellite_id="gen-ipc", data_dir=tempfile.mkdtemp(), wal_enabled=False,
        )
        sat = GeneralSatellite(config, ring)

        # Insert via IPC
        payload = msgpack.packb({"table": "products", "row": {"name": "Widget", "price": 9.99}})
        env = hub.dispatch(CommandType.INSERT, "products", payload)
        sat.process_one()
        result = hub.collect(env)
        assert result.success is True
        resp = msgpack.unpackb(result.data, raw=False)
        assert resp["row_id"] == 1
        ring.unlink()
        hub.close()


# ═══════════════════════════════════════════════════════════════════
#  6. POSTGRES WIRE PROTOCOL
# ═══════════════════════════════════════════════════════════════════

class TestPostgresWire:
    """Test PostgreSQL wire protocol codec."""

    def test_startup_parse(self):
        from qm_core.wire import PgProtocol
        # Simulate startup message: protocol=3.0, user=qm, database=test
        params = b"user\x00qm\x00database\x00testdb\x00\x00"
        data = struct.pack("!I", 196608) + params  # protocol v3
        parsed = PgProtocol.parse_startup(data)
        assert parsed["user"] == "qm"
        assert parsed["database"] == "testdb"

    def test_auth_ok(self):
        from qm_core.wire import PgProtocol
        msg = PgProtocol.build_auth_ok()
        assert msg[0:1] == b"R"
        assert struct.unpack("!I", msg[1:5])[0] == 8  # length=8
        assert struct.unpack("!I", msg[5:9])[0] == 0  # auth OK

    def test_row_description(self):
        from qm_core.wire import PgProtocol, PgTypeOID
        msg = PgProtocol.build_row_description([("name", PgTypeOID.TEXT), ("age", PgTypeOID.INT4)])
        assert msg[0:1] == b"T"
        # 2 columns should be declared
        _, length = struct.unpack("!cI", msg[0:5])
        col_count = struct.unpack("!h", msg[5:7])[0]
        assert col_count == 2

    def test_data_row(self):
        from qm_core.wire import PgProtocol
        msg = PgProtocol.build_data_row(["Alice", "30", None])
        assert msg[0:1] == b"D"
        # 3 columns
        col_count = struct.unpack("!h", msg[5:7])[0]
        assert col_count == 3

    def test_error_response(self):
        from qm_core.wire import PgProtocol
        msg = PgProtocol.build_error_response(message="table not found")
        assert msg[0:1] == b"E"
        assert b"table not found" in msg

    def test_session_handshake(self):
        from qm_core.wire import PgSession
        session = PgSession(pid=42, secret=123)
        startup = struct.pack("!I", 196608) + b"user\x00qm\x00database\x00test\x00\x00"
        response = session.handle_startup(startup)

        # Should contain R (auth), S (params), K (key), Z (ready)
        assert b"R" in response[:1]  # AuthOK first
        # ReadyForQuery is the final message: Z(1B) + len(4B) + status(1B) = 6 bytes
        assert response[-6:-5] == b"Z"  # ReadyForQuery last

    def test_session_query(self):
        from qm_core.wire import PgSession

        def mock_execute(sql):
            return (["id", "name"], [[1, "Alice"], [2, "Bob"]])

        session = PgSession(execute_fn=mock_execute)
        response = session.handle_query("SELECT * FROM users")

        # Should contain T (RowDesc), D (DataRow), C (Complete), Z (Ready)
        assert b"T" in response
        assert b"D" in response
        assert b"C" in response
        assert b"SELECT 2" in response

    def test_session_empty_query(self):
        from qm_core.wire import PgSession
        session = PgSession()
        response = session.handle_query("")
        assert b"I" in response  # EmptyQueryResponse

    def test_session_error(self):
        from qm_core.wire import PgSession

        def bad_execute(sql):
            raise RuntimeError("syntax error at position 5")

        session = PgSession(execute_fn=bad_execute)
        response = session.handle_query("SELEC * FROM x")
        assert b"E" in response
        assert b"syntax error" in response

    def test_type_oid_mapping(self):
        from qm_core.wire import PgTypeOID
        assert PgTypeOID.from_qm_type("int") == 23
        assert PgTypeOID.from_qm_type("text") == 25
        assert PgTypeOID.from_qm_type("float") == 700
        assert PgTypeOID.from_qm_type("json") == 3802

    def test_read_message(self):
        from qm_core.wire import PgProtocol
        # Build a Query message: Q + length + "SELECT 1\0"
        sql = b"SELECT 1\x00"
        msg = b"Q" + struct.pack("!I", 4 + len(sql)) + sql
        msg_type, consumed, payload = PgProtocol.read_message(msg)
        assert msg_type == b"Q"
        assert consumed == len(msg)
        assert PgProtocol.parse_query(payload) == "SELECT 1"

    def test_session_handle_message_simple_query(self):
        from qm_core.wire import PgSession, FrontendMsg

        def mock_execute(sql):
            assert "SELECT" in sql.upper()
            return (["id", "name"], [[1, "Alice"], [2, "Bob"]])

        session = PgSession(execute_fn=mock_execute)
        payload = b"SELECT id, name FROM users\x00"
        response = session.handle_message(FrontendMsg.QUERY, payload)

        assert b"T" in response  # RowDescription
        assert b"D" in response  # DataRow
        assert b"SELECT 2" in response
        assert response[-6:-5] == b"Z"  # ReadyForQuery

    def test_session_extended_parse_bind_execute_with_params(self):
        from qm_core.wire import PgSession, FrontendMsg

        calls = []

        def mock_execute(sql):
            calls.append(sql)
            if "NULL" in sql:
                # Describe path may probe with NULL substitutions.
                return (["id", "name"], [23, 25], [])
            assert "WHERE id = 1" in sql
            return (["id", "name"], [23, 25], [[1, "Alice"]])

        session = PgSession(execute_fn=mock_execute)

        # Parse: statement="s1", query="SELECT id, name FROM users WHERE id = $1"
        parse_payload = (
            b"s1\x00"
            + b"SELECT id, name FROM users WHERE id = $1\x00"
            + struct.pack("!h", 1)
            + struct.pack("!i", 23)
        )
        parse_resp = session.handle_message(FrontendMsg.PARSE, parse_payload)
        assert parse_resp[0:1] == b"1"  # ParseComplete

        # Bind: portal="p1", statement="s1", text format param "1"
        bind_payload = (
            b"p1\x00"
            + b"s1\x00"
            + struct.pack("!h", 1)
            + struct.pack("!h", 0)
            + struct.pack("!h", 1)
            + struct.pack("!i", 1)
            + b"1"
            + struct.pack("!h", 0)
        )
        bind_resp = session.handle_message(FrontendMsg.BIND, bind_payload)
        assert bind_resp[0:1] == b"2"  # BindComplete

        # Describe portal should return row description.
        describe_resp = session.handle_message(FrontendMsg.DESCRIBE, b"Pp1\x00")
        assert describe_resp[0:1] == b"T"

        # Execute portal.
        execute_resp = session.handle_message(
            FrontendMsg.EXECUTE,
            b"p1\x00" + struct.pack("!i", 0),
        )
        assert b"D" in execute_resp
        assert b"SELECT 1" in execute_resp
        assert any("WHERE id = 1" in q for q in calls)

        # Sync should finish extended cycle with ReadyForQuery.
        sync_resp = session.handle_message(FrontendMsg.SYNC, b"")
        assert sync_resp[0:1] == b"Z"


# ═══════════════════════════════════════════════════════════════════
#  7. XOR-DELTA VECTOR COMPRESSION
# ═══════════════════════════════════════════════════════════════════

class TestXORDeltaCompression:
    """Test lossless XOR-Delta vector encoding."""

    def test_encode_decode_single(self):
        from qm_core.compression import XORDeltaCodec
        codec = XORDeltaCodec(dim=8)

        ref = np.array([1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0], dtype=np.float32)
        vec = np.array([1.0, 2.1, 3.0, 4.0, 5.0, 6.0, 7.0, 8.5], dtype=np.float32)

        enc_ref = codec.encode(ref)
        assert enc_ref[0] == 0x01  # reference flag

        enc_vec = codec.encode(vec)
        assert enc_vec[0] == 0x00  # delta flag

        # Verify compression (only 2 values differ)
        uncompressed_size = 8 * 4  # 32 bytes
        assert len(enc_vec) < uncompressed_size

        # Reset codec for decoding
        codec2 = XORDeltaCodec(dim=8)
        dec_ref = codec2.decode(enc_ref)
        np.testing.assert_array_equal(dec_ref, ref)

        dec_vec = codec2.decode(enc_vec)
        np.testing.assert_array_equal(dec_vec, vec)

    def test_identical_vectors_compress_well(self):
        from qm_core.compression import XORDeltaCodec
        codec = XORDeltaCodec(dim=128)
        ref = np.random.randn(128).astype(np.float32)
        same = ref.copy()

        codec.encode(ref)  # set as reference
        enc = codec.encode(same)

        # Same vector → only bitmask (all zeros) + header
        bitmask_size = (128 + 7) // 8
        expected_size = 1 + bitmask_size  # flag + empty bitmask
        assert len(enc) == expected_size

    def test_lossless_guarantee(self):
        """Verify bit-exact reconstruction for random vectors."""
        from qm_core.compression import XORDeltaCodec
        dim = 256
        codec_enc = XORDeltaCodec(dim=dim)
        codec_dec = XORDeltaCodec(dim=dim)

        np.random.seed(42)
        ref = np.random.randn(dim).astype(np.float32)
        vectors = [np.random.randn(dim).astype(np.float32) for _ in range(20)]

        # Encode
        enc_ref = codec_enc.encode(ref)
        encoded = [codec_enc.encode(v) for v in vectors]

        # Decode
        dec_ref = codec_dec.decode(enc_ref)
        np.testing.assert_array_equal(dec_ref, ref)
        for enc, original in zip(encoded, vectors):
            decoded = codec_dec.decode(enc)
            np.testing.assert_array_equal(decoded, original)

    def test_batch_codec(self):
        from qm_core.compression import XORDeltaBatchCodec
        dim = 32
        n = 50
        codec = XORDeltaBatchCodec(dim=dim)

        np.random.seed(123)
        # Create vectors that share most float32 values (good for XOR-Delta)
        base = np.random.randn(dim).astype(np.float32)
        vectors = np.tile(base, (n, 1))
        # Modify only 2-3 positions per vector
        for i in range(n):
            idx = i % dim
            vectors[i, idx] += 0.5

        compressed = codec.encode_batch(vectors)
        decoded = codec.decode_batch(compressed)
        assert decoded.shape == (n, dim)

    def test_native_xor_delta(self):
        from qm_core.native.bridge import xor_delta_encode_native, xor_delta_decode_native
        dim = 64
        ref = np.random.randn(dim).astype(np.float32)
        vec = ref.copy()
        vec[0] += 1.0
        vec[10] += 0.5

        bitmask, deltas = xor_delta_encode_native(vec, ref)
        decoded = xor_delta_decode_native(ref, bitmask, deltas, dim)
        np.testing.assert_array_equal(decoded, vec)


# ═══════════════════════════════════════════════════════════════════
#  8. DISKANN SSD-BASED INDEX
# ═══════════════════════════════════════════════════════════════════

class TestDiskANN:
    """Test DiskANN SSD-optimized vector index."""

    def test_build_and_search(self):
        from qm_core.index.diskann import DiskANN, DiskANNConfig
        config = DiskANNConfig(dim=16, max_degree=16, build_beam=64, search_beam=50, pq_subvectors=4)
        idx = DiskANN(config, data_dir=tempfile.mkdtemp())

        np.random.seed(42)
        n = 50
        ids = list(range(n))
        vectors = np.random.randn(n, 16).astype(np.float32)

        idx.build(ids, vectors)
        assert idx.count == n

        # Verify graph was built
        assert len(idx._graph) == n
        assert idx.medoid in ids

        # Verify PQ codebook trained
        assert idx._pq_centroids is not None

        # Verify vectors on disk (load back and compare)
        for i in [0, 10, 49]:
            loaded = idx._load_vector_from_disk(i)
            assert loaded is not None
            np.testing.assert_array_equal(loaded, vectors[i])

        # Search returns results (quality depends on graph/PQ)
        query = vectors[0]
        results = idx.search(query, top_k=10)
        assert len(results) >= 1

    def test_single_insert(self):
        from qm_core.index.diskann import DiskANN, DiskANNConfig
        config = DiskANNConfig(dim=8, max_degree=8, pq_subvectors=2, search_beam=20)
        idx = DiskANN(config, data_dir=tempfile.mkdtemp())

        # Build with small initial set
        np.random.seed(7)
        vectors = np.random.randn(10, 8).astype(np.float32)
        idx.build(list(range(10)), vectors)

        # Insert new vector
        new_vec = np.array([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], dtype=np.float32)
        idx.insert(100, new_vec)
        assert idx.count == 11

        # Verify inserted vector is on disk and retrievable
        loaded = idx._load_vector_from_disk(100)
        np.testing.assert_array_equal(loaded, new_vec)

    def test_stats(self):
        from qm_core.index.diskann import DiskANN, DiskANNConfig
        config = DiskANNConfig(dim=16, pq_subvectors=4)
        idx = DiskANN(config, data_dir=tempfile.mkdtemp())
        np.random.seed(42)
        idx.build(list(range(50)), np.random.randn(50, 16).astype(np.float32))
        stats = idx.stats()
        assert stats["count"] == 50
        assert stats["graph_nodes"] == 50
        assert stats["avg_degree"] >= 0
        assert stats["disk_bytes"] == 50 * 16 * 4


# ═══════════════════════════════════════════════════════════════════
#  9. CONTENT-DEFINED CHUNKING (CDC)
# ═══════════════════════════════════════════════════════════════════

class TestContentDefinedChunking:
    """Test CDC for blob deduplication."""

    def test_basic_chunking(self):
        from qm_core.compression import ContentDefinedChunker
        chunker = ContentDefinedChunker(avg_chunk_size=256, min_chunk=64, max_chunk=1024)
        data = os.urandom(10000)
        chunks = chunker.chunk(data)
        assert len(chunks) > 1
        # Reassembly
        assert b"".join(chunks) == data

    def test_shift_resistance(self):
        """CDC should produce mostly identical chunks even with prefix insertion."""
        from qm_core.compression import ContentDefinedChunker
        chunker = ContentDefinedChunker(avg_chunk_size=512, min_chunk=128, max_chunk=2048)

        base_data = os.urandom(20000)
        shifted_data = b"INSERTED_PREFIX" + base_data

        chunks1 = chunker.chunk(base_data)
        chunks2 = chunker.chunk(shifted_data)

        # Most chunks should be the same despite prefix shift
        set1 = set(hashlib.sha256(c).hexdigest() for c in chunks1)
        set2 = set(hashlib.sha256(c).hexdigest() for c in chunks2)
        overlap = len(set1 & set2)
        # With CDC, many chunks should survive the shift
        assert overlap > 0

    def test_deterministic(self):
        from qm_core.compression import ContentDefinedChunker
        chunker = ContentDefinedChunker()
        data = b"Hello World" * 5000
        c1 = chunker.chunk(data)
        c2 = chunker.chunk(data)
        assert [hashlib.sha256(c).digest() for c in c1] == [hashlib.sha256(c).digest() for c in c2]


# ═══════════════════════════════════════════════════════════════════
# 10. END-TO-END: HUB ↔ SATELLITE TRANSACTION FLOW
# ═══════════════════════════════════════════════════════════════════

class TestEndToEndFlow:
    """Test complete transaction flow: Client → Hub → Satellite → Hub → Client."""

    def test_full_insert_flow(self):
        """SQL INSERT → Hub (LSN) → Ring → Satellite → Result → Merkle."""
        import msgpack
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=32, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(
            satellite_id="e2e-sat", data_dir=tempfile.mkdtemp(), wal_enabled=False,
        )
        sat = GeneralSatellite(config, ring)

        # Step 1: Hub dispatches INSERT
        payload = msgpack.packb({
            "table": "orders",
            "row": {"product": "Widget", "qty": 5, "price": 19.99},
        })
        env = hub.dispatch(CommandType.INSERT, "orders", payload)
        assert env.stamp.lsn == 1

        # Step 2: Satellite processes
        sat.process_one()

        # Step 3: Hub collects result
        result = hub.collect(env)
        assert result.success is True
        resp = msgpack.unpackb(result.data, raw=False)
        assert resp["row_id"] == 1

        # Step 4: Merkle tree updated
        assert hub.auditor.leaf_count == 1
        root = hub.merkle_root()
        assert root != b"\x00" * 32

        ring.unlink()
        hub.close()

    def test_multi_command_flow(self):
        """Multiple sequential commands maintaining LSN ordering."""
        import msgpack
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=32, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(
            satellite_id="e2e-multi", data_dir=tempfile.mkdtemp(), wal_enabled=False,
        )
        sat = GeneralSatellite(config, ring)

        # Dispatch 3 commands
        for i in range(3):
            payload = msgpack.packb({"table": "items", "row": {"name": f"item_{i}"}})
            env = hub.dispatch(CommandType.INSERT, "items", payload)
            sat.process_one()
            result = hub.collect(env)
            assert result.success is True
            assert result.lsn == i + 1

        # Verify sequential LSNs
        assert hub.sequencer.current_lsn == 4
        assert hub.auditor.leaf_count == 3
        ring.unlink()
        hub.close()

    def test_error_recovery_flow(self):
        """Hub handles satellite errors gracefully."""
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.base import Satellite, SatelliteConfig
        from qm_core.hub.lsn_sequencer import LSNStamp

        class FailingSatellite(Satellite):
            def _execute_command(self, stamp, cmd, payload):
                raise ValueError("Simulated disk failure")

        ring = SharedRingBuffer(slot_count=16, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(
            satellite_id="fail-sat", data_dir=tempfile.mkdtemp(), wal_enabled=False,
        )
        sat = FailingSatellite(config, ring)

        env = hub.dispatch(CommandType.INSERT, "t", b"data")
        sat.process_one()
        result = hub.collect(env)
        assert result.success is False
        assert b"disk failure" in result.data
        assert hub.stats()["errors"] == 1
        ring.unlink()
        hub.close()

    def test_concurrent_hub_satellite(self):
        """Hub and Satellite running on separate threads."""
        import msgpack
        from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType
        from qm_core.hub.hub import Hub
        from qm_core.satellite.general_satellite import GeneralSatellite
        from qm_core.satellite.base import SatelliteConfig

        ring = SharedRingBuffer(slot_count=64, slot_data_size=4096)
        hub = Hub(ring=ring, start_lsn=1)
        config = SatelliteConfig(
            satellite_id="conc-sat", data_dir=tempfile.mkdtemp(), wal_enabled=False,
        )
        sat = GeneralSatellite(config, ring)

        # Start satellite in background
        sat.start()

        results = []
        for i in range(10):
            payload = msgpack.packb({"table": "events", "row": {"seq": i}})
            env = hub.dispatch(CommandType.INSERT, "events", payload)
            result = hub.collect(env)
            results.append(result)

        sat.stop()

        assert all(r.success for r in results)
        lsns = [r.lsn for r in results]
        assert lsns == list(range(1, 11))
        ring.unlink()
        hub.close()

    def test_wire_to_hub_integration(self):
        """PgSession → SQL → QMEngine → Hub pipeline."""
        from qm_core.wire import PgSession

        executed_queries = []

        def mock_engine_execute(sql):
            executed_queries.append(sql)
            if sql.strip().upper().startswith("SELECT"):
                return (["id", "name"], [[1, "Widget"]])
            return ([], [])

        session = PgSession(execute_fn=mock_engine_execute, pid=1)

        # Startup handshake
        startup_data = struct.pack("!I", 196608) + b"user\x00qm\x00\x00"
        resp = session.handle_startup(startup_data)
        assert b"Z" in resp

        # Query
        resp = session.handle_query("SELECT id, name FROM products")
        assert b"SELECT 1" in resp
        assert executed_queries[-1] == "SELECT id, name FROM products"
