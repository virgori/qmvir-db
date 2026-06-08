"""Tests for Hub-Satellite Architecture Compliance.

Validates:
1. Hub is pure control plane — no data computation
2. Satellites run with isolated state
3. All data flows through SharedRingBuffer IPC
4. LSN sequencing for all mutations
5. Merkle root integrity
6. XOR-Delta lossless compression (primary path)
7. ProcedureSatellite executes PL/QM in isolation
8. HubDispatcher routes all ops through Hub
9. SatelliteWorker process model
10. TCP server wires through Hub executor
"""

import os
import struct
import tempfile
import time

import numpy as np
import pytest
import msgpack

from qm_core.ipc.ring_buffer import SharedRingBuffer, CommandType, SlotState
from qm_core.hub.hub import Hub, CommandEnvelope, CommandResult
from qm_core.hub.lsn_sequencer import LSNSequencer, LSNStamp
from qm_core.hub.dispatcher import HubDispatcher, DispatcherConfig
from qm_core.satellite.base import Satellite, SatelliteConfig
from qm_core.satellite.general_satellite import GeneralSatellite
from qm_core.satellite.vector_satellite import VectorSatellite, _pack_vector, _pack_search
from qm_core.satellite.procedure_satellite import ProcedureSatellite
from qm_core.satellite.worker import SatelliteWorker, SatelliteCluster, WorkerSpec
from qm_core.compression import XORDeltaCodec, XORDeltaBatchCodec


# ── Fixtures ────────────────────────────────────────────────────────

@pytest.fixture
def tmp_ring(tmp_path):
    """Create a fresh SharedRingBuffer."""
    path = str(tmp_path / "test_ring.shm")
    ring = SharedRingBuffer(path=path, slot_count=64, slot_data_size=8192, create=True)
    yield ring


@pytest.fixture
def hub_and_sat(tmp_ring, tmp_path):
    """Create a Hub + GeneralSatellite pair connected via ring."""
    hub = Hub(ring=tmp_ring)
    sat = GeneralSatellite(
        SatelliteConfig(satellite_id="gen-test", data_dir=str(tmp_path / "sat")),
        tmp_ring,
    )
    sat.start()
    yield hub, sat
    sat.stop()


@pytest.fixture
def dispatcher(tmp_path):
    """Create a HubDispatcher with in-process satellites."""
    d = HubDispatcher(DispatcherConfig(
        ring_dir=str(tmp_path / "rings"),
        slot_count=64,
        slot_data_size=8192,
    ))

    sat = GeneralSatellite(
        SatelliteConfig(satellite_id="gen-disp", data_dir=str(tmp_path / "sat")),
        d.ring_gen,
    )
    sat.start()
    yield d, sat
    sat.stop()
    d.close()


# ═══════════════════════════════════════════════════════════════════
# 1. Hub is Pure Control Plane
# ═══════════════════════════════════════════════════════════════════

class TestHubControlPlane:
    """Hub MUST only assign LSNs, coordinate, and audit — never compute data."""

    def test_hub_assigns_monotonic_lsn(self, hub_and_sat):
        hub, sat = hub_and_sat
        payload1 = msgpack.packb({"table": "t1", "row": {"x": 1}})
        payload2 = msgpack.packb({"table": "t1", "row": {"x": 2}})

        env1 = hub.dispatch(CommandType.INSERT, "t1", payload1)
        env2 = hub.dispatch(CommandType.INSERT, "t1", payload2)

        assert env2.stamp.lsn > env1.stamp.lsn
        assert env2.stamp.lsn == env1.stamp.lsn + 1

        # Collect to clean up
        hub.collect(env1)
        hub.collect(env2)

    def test_hub_maintains_merkle_root(self, hub_and_sat):
        hub, sat = hub_and_sat
        payload = msgpack.packb({"table": "t1", "row": {"x": 1}})

        root_before = hub.merkle_root()
        result = hub.dispatch_sync(CommandType.INSERT, "t1", payload)
        root_after = hub.merkle_root()

        assert result.success
        # Merkle root changes after write
        assert root_after != root_before

    def test_hub_tracks_stats(self, hub_and_sat):
        hub, sat = hub_and_sat
        payload = msgpack.packb({"table": "t1", "row": {"x": 1}})

        hub.dispatch_sync(CommandType.INSERT, "t1", payload)
        stats = hub.stats()

        assert stats["dispatched"] >= 1
        assert stats["completed"] >= 1

    def test_hub_no_data_in_memory(self, hub_and_sat):
        """Hub should not hold any raw data — only metadata."""
        hub, sat = hub_and_sat
        # Hub should not have rows, vectors, tables, or indexes
        assert not hasattr(hub, "_tables")
        assert not hasattr(hub, "_rows")
        assert not hasattr(hub, "_vectors")
        assert not hasattr(hub, "_indexes")


# ═══════════════════════════════════════════════════════════════════
# 2. Satellite Isolation
# ═══════════════════════════════════════════════════════════════════

class TestSatelliteIsolation:
    """Satellites must be independently operational with own state."""

    def test_general_satellite_processes_insert(self, hub_and_sat):
        hub, sat = hub_and_sat
        payload = msgpack.packb({"table": "users", "row": {"name": "Alice", "age": 30}})
        result = hub.dispatch_sync(CommandType.INSERT, "users", payload)

        assert result.success
        resp = msgpack.unpackb(result.data, raw=False)
        assert resp["row_id"] == 1
        assert resp["lsn"] > 0

    def test_general_satellite_processes_query(self, hub_and_sat):
        hub, sat = hub_and_sat
        # Insert first
        payload = msgpack.packb({"table": "items", "row": {"name": "Widget", "price": 10}})
        hub.dispatch_sync(CommandType.INSERT, "items", payload)

        # Query
        qpayload = msgpack.packb({"table": "items", "predicates": []})
        result = hub.dispatch_sync(CommandType.QUERY, "items", qpayload)

        assert result.success
        resp = msgpack.unpackb(result.data, raw=False)
        rows = resp.get("rows", [])
        assert len(rows) == 1
        assert rows[0]["name"] == "Widget"

    def test_satellite_carries_lsn(self, hub_and_sat):
        """Each mutation in the satellite MUST carry LSN from Hub."""
        hub, sat = hub_and_sat
        payload = msgpack.packb({"table": "t1", "row": {"val": 42}})
        result = hub.dispatch_sync(CommandType.INSERT, "t1", payload)

        resp = msgpack.unpackb(result.data, raw=False)
        assert "lsn" in resp
        assert resp["lsn"] > 0

    def test_satellite_stats(self, hub_and_sat):
        hub, sat = hub_and_sat
        payload = msgpack.packb({"table": "t1", "row": {"x": 1}})
        hub.dispatch_sync(CommandType.INSERT, "t1", payload)

        stats = sat.stats()
        assert stats["processed"] >= 1
        assert stats["last_lsn"] > 0


# ═══════════════════════════════════════════════════════════════════
# 3. Ring Buffer IPC
# ═══════════════════════════════════════════════════════════════════

class TestRingBufferIPC:
    """All data MUST flow through SharedRingBuffer."""

    def test_ring_publish_consume_cycle(self, tmp_ring):
        """Basic publish → consume → complete cycle."""
        ring = tmp_ring

        # Publish (Hub side)
        stamp = LSNStamp(lsn=1, epoch=0, timestamp_ns=time.time_ns())
        payload = stamp.to_bytes() + b"hello"
        slot = ring.try_publish(lsn=1, cmd=CommandType.INSERT, payload=payload)
        assert slot >= 0

        # Consume (Satellite side)
        result = ring.consume()
        assert result is not None
        slot_idx, hdr, data_mv = result
        assert hdr.cmd == CommandType.INSERT

        # Complete
        ring.complete(slot_idx, result_payload=b"OK")
        state, data = ring.collect_result(slot_idx)
        assert state == SlotState.DONE

    def test_ring_error_path(self, tmp_ring):
        """Satellite can fail a slot."""
        ring = tmp_ring
        stamp = LSNStamp(lsn=1, epoch=0, timestamp_ns=time.time_ns())
        payload = stamp.to_bytes() + b"bad"
        slot = ring.try_publish(lsn=1, cmd=CommandType.INSERT, payload=payload)

        result = ring.consume()
        slot_idx, _, _ = result
        ring.fail(slot_idx, error_payload=b"error msg")

        state, data = ring.collect_result(slot_idx)
        assert state == SlotState.ERROR

    def test_ring_buffer_stats(self, tmp_ring):
        stats = tmp_ring.stats()
        # Stats returns slot state counts
        assert "EMPTY" in stats or "slot_count" in stats


# ═══════════════════════════════════════════════════════════════════
# 4. LSN Sequencing
# ═══════════════════════════════════════════════════════════════════

class TestLSNSequencing:
    """All mutations MUST have deterministic LSN from Hub."""

    def test_lsn_monotonic(self):
        seq = LSNSequencer(start_lsn=1, epoch=0)
        stamps = seq.next_batch(10)
        lsns = [s.lsn for s in stamps]
        assert lsns == list(range(1, 11))

    def test_lsn_stamp_serialization(self):
        stamp = LSNStamp(lsn=42, epoch=1, timestamp_ns=time.time_ns())
        data = stamp.to_bytes()
        assert len(data) == 24  # 8 + 8 + 8
        restored = LSNStamp.from_bytes(data)
        assert restored.lsn == 42
        assert restored.epoch == 1

    def test_lsn_gap_free(self):
        seq = LSNSequencer(start_lsn=100, epoch=0)
        for expected in range(100, 110):
            s = seq.next()
            assert s.lsn == expected

    def test_lsn_epoch_bump(self):
        seq = LSNSequencer(start_lsn=1, epoch=0)
        seq.bump_epoch()
        s = seq.next()
        assert s.epoch == 1


# ═══════════════════════════════════════════════════════════════════
# 5. HubDispatcher
# ═══════════════════════════════════════════════════════════════════

class TestHubDispatcher:
    """HubDispatcher routes all ops through Hub IPC."""

    def test_dispatcher_insert_and_query(self, dispatcher):
        d, sat = dispatcher
        result = d.insert("mydb", {"name": "Test", "value": 99})
        assert result.success

        qresult = d.query("mydb")
        assert qresult.success
        resp = msgpack.unpackb(qresult.data, raw=False)
        rows = resp.get("rows", [])
        assert len(rows) == 1
        assert rows[0]["name"] == "Test"

    def test_dispatcher_assigns_lsn(self, dispatcher):
        d, sat = dispatcher
        initial_lsn = d.current_lsn
        d.insert("t1", {"x": 1})
        d.insert("t1", {"x": 2})
        assert d.current_lsn >= initial_lsn + 2

    def test_dispatcher_create_table(self, dispatcher):
        d, sat = dispatcher
        result = d.create_table("newtable", {"col1": "text"})
        assert result.success

    def test_dispatcher_update(self, dispatcher):
        d, sat = dispatcher
        d.insert("t1", {"val": 1})
        result = d.update("t1", 1, {"val": 2})
        assert result.success

    def test_dispatcher_delete(self, dispatcher):
        d, sat = dispatcher
        d.insert("t1", {"val": 1})
        result = d.delete("t1", 1)
        assert result.success

    def test_dispatcher_batch_insert(self, dispatcher):
        d, sat = dispatcher
        results = d.insert_batch("t1", [{"x": i} for i in range(5)])
        assert len(results) == 5
        assert all(r.success for r in results)


# ═══════════════════════════════════════════════════════════════════
# 6. XOR-Delta Lossless Compression (Primary Path)
# ═══════════════════════════════════════════════════════════════════

class TestXORDeltaLossless:
    """XOR-Delta MUST be the primary compression — not PQ (lossy)."""

    def test_codec_lossless_roundtrip(self):
        dim = 128
        codec = XORDeltaCodec(dim)
        ref = np.random.randn(dim).astype(np.float32)
        codec.set_reference(ref)

        vec = ref + np.random.randn(dim).astype(np.float32) * 0.01
        encoded = codec.encode(vec)
        decoded = codec.decode(encoded)

        np.testing.assert_array_equal(vec.astype(np.float32), decoded)

    def test_batch_codec_lossless(self):
        dim = 64
        n = 20
        codec = XORDeltaBatchCodec(dim)
        vectors = np.random.randn(n, dim).astype(np.float32)

        compressed = codec.encode_batch(vectors)
        recovered = codec.decode_batch(compressed)

        np.testing.assert_array_equal(vectors, recovered)

    def test_compression_ratio(self):
        """XOR-Delta codec compresses well when reference matches exactly."""
        dim = 128
        codec = XORDeltaCodec(dim)
        ref = np.ones(dim, dtype=np.float32) * 3.14
        codec.set_reference(ref)

        # Identical vector → only bitmask overhead, no deltas
        encoded_same = codec.encode(ref.copy())
        # flag(1) + bitmask(16 all zeros) = 17 bytes vs raw 512 bytes
        assert len(encoded_same) < dim * 4

        # Nearly identical → few delta bytes
        perturbed = ref.copy()
        perturbed[0] += 0.001  # Only 1 dim differs
        encoded_diff = codec.encode(perturbed)
        # flag(1) + bitmask(16) + 1 delta(4) = 21 bytes
        assert len(encoded_diff) < dim * 4 // 2

        # Verify lossless
        decoded = codec.decode(encoded_diff)
        np.testing.assert_array_equal(perturbed, decoded)

    def test_vector_satellite_uses_xor_delta(self, tmp_path):
        """VectorSatellite MUST use XOR-Delta for primary storage."""
        ring_path = str(tmp_path / "vec_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=8192, create=True)
        sat = VectorSatellite(
            SatelliteConfig(satellite_id="vec-test", data_dir=str(tmp_path / "vsat")),
            ring, dim=32,
        )
        assert sat._compression_enabled is True
        assert isinstance(sat._xor_codec, XORDeltaCodec)

        # Insert should populate compressed store
        vec = np.random.randn(32).astype(np.float32)
        sat.insert_vector(1, vec)
        assert 1 in sat._compressed_store

    def test_vector_satellite_compress_all(self, tmp_path):
        ring_path = str(tmp_path / "vec_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=8192, create=True)
        dim = 64
        sat = VectorSatellite(
            SatelliteConfig(satellite_id="vec-test", data_dir=str(tmp_path / "vsat")),
            ring, dim=dim,
        )
        # Insert tightly clustered vectors for good compression
        base = np.ones(dim, dtype=np.float32)
        for i in range(10):
            vec = base.copy()
            vec[i % dim] += 0.01
            sat.insert_vector(i, vec)

        stats = sat.compress_all()
        assert stats["compressed"] == 10
        assert stats["ratio"] > 0  # Lossless codec engaged


# ═══════════════════════════════════════════════════════════════════
# 7. Procedure Satellite
# ═══════════════════════════════════════════════════════════════════

class TestProcedureSatellite:
    """ProcedureSatellite executes PL/QM in isolated process."""

    def test_register_and_call(self, tmp_path):
        ring_path = str(tmp_path / "proc_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=8192, create=True)
        hub = Hub(ring=ring)
        sat = ProcedureSatellite(
            SatelliteConfig(satellite_id="plqm-test", data_dir=str(tmp_path / "psat")),
            ring,
        )
        sat.start()

        try:
            # Register procedure
            reg_payload = msgpack.packb({
                "action": "register",
                "name": "add_numbers",
                "body": "DECLARE result = a + b;\nRETURN result;",
                "params": [
                    {"name": "a", "type": "INT", "required": True},
                    {"name": "b", "type": "INT", "required": True},
                ],
            })
            result = hub.dispatch_sync(CommandType.DDL, "_plqm", reg_payload)
            assert result.success
            resp = msgpack.unpackb(result.data, raw=False)
            assert resp["registered"] == "add_numbers"

            # Call procedure
            call_payload = msgpack.packb({
                "name": "add_numbers",
                "args": {"a": 3, "b": 7},
            })
            result = hub.dispatch_sync(CommandType.DDL, "_plqm", call_payload)
            assert result.success
            resp = msgpack.unpackb(result.data, raw=False)
            assert resp["result"] == 10
            assert resp["success"] is True
        finally:
            sat.stop()

    def test_procedure_list(self, tmp_path):
        ring_path = str(tmp_path / "proc_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=8192, create=True)
        hub = Hub(ring=ring)
        sat = ProcedureSatellite(
            SatelliteConfig(satellite_id="plqm-test", data_dir=str(tmp_path / "psat")),
            ring,
        )
        sat.start()

        try:
            # Register
            reg_payload = msgpack.packb({
                "action": "register",
                "name": "test_proc",
                "body": "RETURN 42;",
                "params": [],
            })
            hub.dispatch_sync(CommandType.DDL, "_plqm", reg_payload)

            # List
            list_payload = msgpack.packb({"action": "list"})
            result = hub.dispatch_sync(CommandType.DDL, "_plqm", list_payload)
            assert result.success
            resp = msgpack.unpackb(result.data, raw=False)
            assert len(resp["procedures"]) >= 1
        finally:
            sat.stop()


# ═══════════════════════════════════════════════════════════════════
# 8. Vector Satellite IPC
# ═══════════════════════════════════════════════════════════════════

class TestVectorSatelliteIPC:
    """Vector operations MUST flow through IPC ring."""

    def test_vector_insert_via_ipc(self, tmp_path):
        ring_path = str(tmp_path / "vec_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=16384, create=True)
        hub = Hub(ring=ring)
        sat = VectorSatellite(
            SatelliteConfig(satellite_id="vec-ipc", data_dir=str(tmp_path / "vsat")),
            ring, dim=16,
        )
        sat.start()

        try:
            vec = np.random.randn(16).astype(np.float32)
            payload = _pack_vector(1, vec, meta=b"test")
            result = hub.dispatch_sync(CommandType.VECTOR_OP, "vectors", payload)

            assert result.success
            vid = struct.unpack("<q", result.data)[0]
            assert vid == 1
            assert sat.vector_count == 1
        finally:
            sat.stop()

    def test_vector_search_via_ipc(self, tmp_path):
        ring_path = str(tmp_path / "vec_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=16384, create=True)
        hub = Hub(ring=ring)
        sat = VectorSatellite(
            SatelliteConfig(satellite_id="vec-ipc", data_dir=str(tmp_path / "vsat")),
            ring, dim=16,
        )
        sat.start()

        try:
            # Insert vectors
            for i in range(5):
                vec = np.random.randn(16).astype(np.float32)
                payload = _pack_vector(i + 1, vec)
                hub.dispatch_sync(CommandType.VECTOR_OP, "vectors", payload)

            # Search
            query = np.random.randn(16).astype(np.float32)
            search_payload = _pack_search(query, top_k=3)
            result = hub.dispatch_sync(CommandType.VECTOR_OP, "vectors", search_payload)

            assert result.success
            count = struct.unpack("<I", result.data[:4])[0]
            assert count == 3
        finally:
            sat.stop()


# ═══════════════════════════════════════════════════════════════════
# 9. SatelliteWorker Process Model
# ═══════════════════════════════════════════════════════════════════

class TestSatelliteWorkerModel:
    """SatelliteWorker manages satellite OS processes."""

    def test_worker_spec_creation(self):
        spec = WorkerSpec(
            satellite_id="vec-0",
            satellite_type="vector",
            ring_path="/tmp/test_ring.shm",
            dim=128,
        )
        assert spec.satellite_id == "vec-0"
        assert spec.satellite_type == "vector"
        assert spec.dim == 128

    def test_cluster_registration(self):
        cluster = SatelliteCluster()
        cluster.add(WorkerSpec("vec-0", "vector", "/tmp/ring.shm"))
        cluster.add(WorkerSpec("gen-0", "general", "/tmp/ring.shm"))
        assert len(cluster.workers) == 2

    def test_worker_not_alive_before_spawn(self):
        spec = WorkerSpec("test-0", "general", "/tmp/ring.shm")
        worker = SatelliteWorker(spec)
        assert not worker.is_alive()
        assert worker.pid is None


# ═══════════════════════════════════════════════════════════════════
# 10. Hub Engine Integration
# ═══════════════════════════════════════════════════════════════════

class TestHubEngineIntegration:
    """QMHubEngine routes everything through Hub IPC."""

    def test_hub_engine_create_and_insert(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("users", {"name": "text", "age": "int"})
            rid = engine.insert("users", {"name": "Alice", "age": 30})
            assert rid >= 1
        finally:
            engine.close()

    def test_hub_engine_find(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("items", {"name": "text", "price": "int"})
            engine.insert("items", {"name": "A", "price": 10})
            engine.insert("items", {"name": "B", "price": 20})

            rows = engine.find("items")
            assert len(rows) == 2
        finally:
            engine.close()

    def test_hub_engine_sql_select(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("t", {"x": "int"})
            engine.insert("t", {"x": 1})
            engine.insert("t", {"x": 2})

            results = engine.execute_sql("SELECT * FROM t")
            assert len(results) == 2
        finally:
            engine.close()

    def test_hub_engine_sql_insert(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("t", {"name": "text", "val": "int"})
            result = engine.execute_sql("INSERT INTO t (name, val) VALUES ('hi', 42)")
            assert result[0]["inserted"] == 1
        finally:
            engine.close()

    def test_hub_engine_lsn_tracking(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("t", {"x": "int"})
            lsn0 = engine.current_lsn
            engine.insert("t", {"x": 1})
            engine.insert("t", {"x": 2})
            assert engine.current_lsn >= lsn0 + 2
        finally:
            engine.close()

    def test_hub_engine_merkle_changes(self, tmp_path):
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            engine.create_table("t", {"x": "int"})
            root1 = engine.merkle_root
            engine.insert("t", {"x": 1})
            root2 = engine.merkle_root
            assert root2 != root1
        finally:
            engine.close()


# ═══════════════════════════════════════════════════════════════════
# 11. Architecture Boundary Tests
# ═══════════════════════════════════════════════════════════════════

class TestArchitectureBoundary:
    """Verify architectural boundaries are enforced."""

    def test_hub_engine_has_no_direct_indexes(self, tmp_path):
        """Hub engine holds no BPlusTree, HNSWIndex, InvertedIndex."""
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            assert not hasattr(engine, '_tables') or not any(
                hasattr(v, 'hnsw') for v in getattr(engine, '_tables', {}).values()
            )
        finally:
            engine.close()

    def test_hub_engine_uses_dispatcher(self, tmp_path):
        """Engine MUST use HubDispatcher for data operations."""
        from qm_core.hub_engine import QMHubEngine
        engine = QMHubEngine(data_dir=str(tmp_path / "hdb"))
        try:
            assert hasattr(engine, '_dispatcher')
            assert isinstance(engine._dispatcher, HubDispatcher)
        finally:
            engine.close()

    def test_ipc_path_not_direct_calls(self, tmp_path):
        """Verify data flows through ring, not direct calls."""
        ring_path = str(tmp_path / "boundary_ring.shm")
        ring = SharedRingBuffer(path=ring_path, slot_count=32, slot_data_size=8192, create=True)
        hub = Hub(ring=ring)
        sat = GeneralSatellite(
            SatelliteConfig(satellite_id="gen-bound", data_dir=str(tmp_path / "sat")),
            ring,
        )
        sat.start()
        try:
            # Insert through Hub IPC
            payload = msgpack.packb({"table": "t", "row": {"k": "v"}})
            result = hub.dispatch_sync(CommandType.INSERT, "t", payload)

            # The hub dispatched count should increase
            stats = hub.stats()
            assert stats["dispatched"] >= 1
            assert stats["completed"] >= 1

            # The satellite processed count should increase
            sat_stats = sat.stats()
            assert sat_stats["processed"] >= 1
        finally:
            sat.stop()

    def test_wire_protocol_executor_factory(self):
        """Hub executor factory creates proper callable."""
        from gateway.api_postgres.hub_executor import make_hub_executor

        class MockEngine:
            def execute_sql(self, sql):
                return [{"id": 1, "name": "test"}]

        executor = make_hub_executor(MockEngine())
        cols, rows = executor("SELECT * FROM t")
        assert cols == ["id", "name"]
        assert rows == [[1, "test"]]
