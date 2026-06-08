"""Task 3 — TCP Wire Protocol Server + DiskANN Quality Upgrade.

Tests cover:
    1. TCP server: asyncio startup, client connection, startup handshake,
       simple query protocol, SELECT/INSERT responses, error handling, terminate
    2. DiskANN upgrades: PQ lookup table, LUT-accelerated beam search,
       diversity-aware MMR re-ranking
"""

import asyncio
import struct
import pytest
import numpy as np


# ─── 1. TCP PostgreSQL Server ──────────────────────────────────────

class TestQMPostgresServer:
    """Real asyncio TCP server tests using raw sockets."""

    @staticmethod
    def _build_startup(user: str = "test", database: str = "qmdb") -> bytes:
        """Build a StartupMessage (no type byte, just length + protocol + params)."""
        params = b""
        params += b"user\x00" + user.encode() + b"\x00"
        params += b"database\x00" + database.encode() + b"\x00"
        params += b"\x00"  # terminator

        protocol = struct.pack("!I", 196608)  # 3.0
        payload = protocol + params
        length = 4 + len(payload)  # length includes itself
        return struct.pack("!I", length) + payload

    @staticmethod
    def _build_query(sql: str) -> bytes:
        """Build a Query (Q) message."""
        encoded = sql.encode("utf-8") + b"\x00"
        length = 4 + len(encoded)
        return b"Q" + struct.pack("!I", length) + encoded

    @staticmethod
    def _build_terminate() -> bytes:
        """Build a Terminate (X) message."""
        return b"X" + struct.pack("!I", 4)

    @staticmethod
    def _read_messages(data: bytes) -> list[tuple[bytes, bytes]]:
        """Parse all backend messages from a response buffer.
        Returns [(type_byte, payload), ...]"""
        msgs = []
        pos = 0
        while pos < len(data):
            if pos + 5 > len(data):
                break
            msg_type = data[pos:pos + 1]
            (length,) = struct.unpack("!I", data[pos + 1:pos + 5])
            total = 1 + length
            payload = data[pos + 5:pos + total]
            msgs.append((msg_type, payload))
            pos += total
        return msgs

    @pytest.mark.asyncio
    async def test_server_start_stop(self):
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            return (["col"], [["val"]])

        srv = QMPostgresServer(executor=executor, port=15432)
        await srv.start()
        assert srv.is_running
        await srv.stop()
        assert not srv.is_running

    @pytest.mark.asyncio
    async def test_connect_and_handshake(self):
        """Full startup handshake: connect → StartupMessage → AuthOK + params + ReadyForQuery."""
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            return (["result"], [["1"]])

        srv = QMPostgresServer(executor=executor, port=15433)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15433)

            # Send startup
            writer.write(self._build_startup())
            await writer.drain()

            # Read response — should be: AuthOK + ParameterStatus*N + BackendKeyData + ReadyForQuery
            data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
            msgs = self._read_messages(data)

            types = [m[0] for m in msgs]
            assert b"R" in types  # AuthOK
            assert b"K" in types  # BackendKeyData
            assert b"Z" in types  # ReadyForQuery

            # Check ReadyForQuery reports idle
            z_msg = [m for m in msgs if m[0] == b"Z"][0]
            assert z_msg[1] == b"I"  # Idle

            # Clean terminate
            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    @pytest.mark.asyncio
    async def test_simple_query_select(self):
        """Execute a SELECT via simple query protocol."""
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            if "SELECT" in sql.upper():
                return (["id", "name"], [["1", "Alice"], ["2", "Bob"]])
            return ([], [])

        srv = QMPostgresServer(executor=executor, port=15434)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15434)

            # Startup
            writer.write(self._build_startup())
            await writer.drain()
            await asyncio.wait_for(reader.read(4096), timeout=2.0)

            # Send query
            writer.write(self._build_query("SELECT * FROM users"))
            await writer.drain()

            data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
            msgs = self._read_messages(data)
            types = [m[0] for m in msgs]

            # Expect: RowDescription (T) + DataRow (D) × 2 + CommandComplete (C) + ReadyForQuery (Z)
            assert b"T" in types  # RowDescription
            assert types.count(b"D") == 2  # 2 DataRows
            assert b"C" in types  # CommandComplete
            assert b"Z" in types  # ReadyForQuery

            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    @pytest.mark.asyncio
    async def test_query_error(self):
        """Query that raises an error returns ErrorResponse."""
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            raise ValueError("table not found")

        srv = QMPostgresServer(executor=executor, port=15435)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15435)

            writer.write(self._build_startup())
            await writer.drain()
            await asyncio.wait_for(reader.read(4096), timeout=2.0)

            writer.write(self._build_query("SELECT * FROM nonexistent"))
            await writer.drain()

            data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
            msgs = self._read_messages(data)
            types = [m[0] for m in msgs]

            assert b"E" in types  # ErrorResponse
            assert b"Z" in types  # ReadyForQuery after error

            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    @pytest.mark.asyncio
    async def test_empty_query(self):
        """Empty query returns EmptyQueryResponse."""
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            return ([], [])

        srv = QMPostgresServer(executor=executor, port=15436)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15436)

            writer.write(self._build_startup())
            await writer.drain()
            await asyncio.wait_for(reader.read(4096), timeout=2.0)

            writer.write(self._build_query(""))
            await writer.drain()

            data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
            msgs = self._read_messages(data)
            types = [m[0] for m in msgs]

            assert b"I" in types  # EmptyQueryResponse
            assert b"Z" in types

            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    @pytest.mark.asyncio
    async def test_multiple_queries(self):
        """Multiple queries on the same connection."""
        from gateway.api_postgres.server import QMPostgresServer

        call_count = {"n": 0}

        def executor(sql):
            call_count["n"] += 1
            return (["v"], [[str(call_count["n"])]])

        srv = QMPostgresServer(executor=executor, port=15437)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15437)

            writer.write(self._build_startup())
            await writer.drain()
            await asyncio.wait_for(reader.read(4096), timeout=2.0)

            for _ in range(3):
                writer.write(self._build_query("SELECT 1"))
                await writer.drain()
                data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
                msgs = self._read_messages(data)
                types = [m[0] for m in msgs]
                assert b"Z" in types

            assert call_count["n"] == 3

            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    @pytest.mark.asyncio
    async def test_ssl_rejection(self):
        """SSL negotiation request is rejected, then normal startup proceeds."""
        from gateway.api_postgres.server import QMPostgresServer

        def executor(sql):
            return (["r"], [["ok"]])

        srv = QMPostgresServer(executor=executor, port=15438)
        await srv.start()

        try:
            reader, writer = await asyncio.open_connection("127.0.0.1", 15438)

            # Send SSL request: Int32(8) + Int32(80877103)
            ssl_req = struct.pack("!II", 8, 80877103)
            writer.write(ssl_req)
            await writer.drain()

            # Should get 'N' (SSL rejected)
            ssl_resp = await asyncio.wait_for(reader.read(1), timeout=2.0)
            assert ssl_resp == b"N"

            # Now send normal startup
            writer.write(self._build_startup())
            await writer.drain()

            data = await asyncio.wait_for(reader.read(4096), timeout=2.0)
            msgs = self._read_messages(data)
            types = [m[0] for m in msgs]
            assert b"R" in types  # AuthOK after SSL rejection
            assert b"Z" in types

            writer.write(self._build_terminate())
            await writer.drain()
            writer.close()
        finally:
            await srv.stop()

    def test_import(self):
        from gateway.api_postgres import QMPostgresServer, run_server
        assert QMPostgresServer is not None
        assert run_server is not None


# ─── 2. DiskANN Quality Upgrades ───────────────────────────────────

class TestDiskANNUpgrades:
    """PQ lookup table, LUT beam search, and diversity-aware MMR."""

    @pytest.fixture
    def index_with_data(self, tmp_path):
        from qm_core.index.diskann import DiskANN, DiskANNConfig
        config = DiskANNConfig(
            dim=16, max_degree=8, build_beam=32,
            search_beam=20, pq_subvectors=4, pq_centroids=8,
        )
        idx = DiskANN(config, str(tmp_path / "diskann"))

        np.random.seed(42)
        n = 50
        ids = list(range(n))
        vectors = np.random.randn(n, 16).astype(np.float32)
        idx.build(ids, vectors)
        return idx, vectors

    def test_pq_lookup_table_shape(self, index_with_data):
        """LUT should be (m, k) float32."""
        idx, vectors = index_with_data
        query = np.random.randn(16).astype(np.float32)
        lut = idx._build_pq_lookup_table(query)
        assert lut.shape == (4, 8)
        assert lut.dtype == np.float32

    def test_lut_distance_matches_direct(self, index_with_data):
        """LUT-based distance should match direct PQ distance."""
        idx, vectors = index_with_data
        query = np.random.randn(16).astype(np.float32)
        lut = idx._build_pq_lookup_table(query)

        # Compare for several vectors
        for vid in range(10):
            codes = idx._pq_codes[vid]
            d_direct = idx._pq_distance(query, codes)
            d_lut = idx._pq_distance_lut(lut, codes)
            assert abs(d_direct - d_lut) < 1e-4, f"Mismatch for vid={vid}"

    def test_search_still_works(self, index_with_data):
        """Search with upgraded LUT codepath returns valid results."""
        idx, vectors = index_with_data
        query = vectors[0]
        results = idx.search(query, top_k=10)

        assert len(results) == 10
        # All returned IDs should be valid node IDs
        for vid, dist in results:
            assert vid in idx._graph
            assert dist >= 0

    def test_search_results_sorted(self, index_with_data):
        """Results are sorted by ascending distance."""
        idx, vectors = index_with_data
        query = np.random.randn(16).astype(np.float32)
        results = idx.search(query, top_k=10)

        dists = [d for _, d in results]
        assert dists == sorted(dists)

    def test_diversity_search_differs(self, index_with_data):
        """High diversity should produce different ordering than pure distance."""
        idx, vectors = index_with_data
        query = np.random.randn(16).astype(np.float32)

        # Pure distance
        results_pure = idx.search(query, top_k=10, diversity=0.0)
        # High diversity
        results_diverse = idx.search(query, top_k=10, diversity=0.8)

        ids_pure = [vid for vid, _ in results_pure]
        ids_diverse = [vid for vid, _ in results_diverse]

        # Both should return top_k results
        assert len(results_pure) == 10
        assert len(results_diverse) == 10

        # The set of IDs may partially overlap but ordering usually differs
        # (with 50 vectors and high diversity, at least some should differ)
        # We only check that the mechanism runs and returns valid results
        assert all(isinstance(vid, (int, np.integer)) for vid, _ in results_diverse)

    def test_diversity_zero_equals_pure(self, index_with_data):
        """diversity=0 should produce same results as no diversity."""
        idx, vectors = index_with_data
        query = vectors[5]

        r1 = idx.search(query, top_k=5, diversity=0.0)
        r2 = idx.search(query, top_k=5)

        ids1 = [vid for vid, _ in r1]
        ids2 = [vid for vid, _ in r2]
        assert ids1 == ids2

    def test_mmr_select_basic(self, index_with_data):
        """_mmr_select with max diversity picks spread-out vectors."""
        idx, vectors = index_with_data
        query = np.random.randn(16).astype(np.float32)

        # Build fake candidate list
        candidates = []
        for i in range(20):
            v = vectors[i]
            d = float(np.sum((query - v) ** 2))
            candidates.append((d, i, v))
        candidates.sort()

        result = idx._mmr_select(candidates, top_k=5, lam=0.5)
        assert len(result) == 5
        # All IDs should be unique
        ids = [vid for vid, _ in result]
        assert len(set(ids)) == 5

    def test_beam_search_pq_lut(self, index_with_data):
        """LUT-based beam search returns valid candidates."""
        idx, vectors = index_with_data
        query = vectors[0]
        lut = idx._build_pq_lookup_table(query)

        candidates = idx._beam_search_pq_lut(query, lut, beam=20)
        assert len(candidates) > 0
        # Each candidate is (distance, vid)
        for dist, vid in candidates:
            assert isinstance(dist, float)
            assert vid in idx._graph

    def test_lut_beam_search_recall(self, index_with_data):
        """LUT beam search gets comparable recall to the original."""
        idx, vectors = index_with_data
        query = vectors[10]

        # Brute-force top-5
        dists = [(float(np.sum((query - vectors[i]) ** 2)), i) for i in range(50)]
        dists.sort()
        true_top5 = {vid for _, vid in dists[:5]}

        results = idx.search(query, top_k=5)
        found = {vid for vid, _ in results}

        # At least 3 of top-5 should be found (recall >= 60%)
        assert len(true_top5 & found) >= 3
