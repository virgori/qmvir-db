"""QM Distributed — TCP Transport for Raft RPCs.

Provides a TCP-based transport that can replace InMemoryRaftTransport for
production deployments.  Messages use length-prefixed JSON for simplicity
and portability.

Wire format:
    [4-byte big-endian length][JSON payload]

Security note: This transport is intended for use within a trusted network
(e.g. a VPC).  For untrusted networks, wrap with TLS.
"""

from __future__ import annotations

import json
import logging
import socket
import struct
import threading
from typing import Any

from qm_core.distributed.consensus import (
    AppendEntriesRequest,
    AppendEntriesResponse,
    LogEntry,
    RaftNode,
    RaftTransport,
    VoteRequest,
    VoteResponse,
)

logger = logging.getLogger(__name__)

# Maximum message size (16 MiB) to prevent memory exhaustion from malformed frames.
MAX_MSG_SIZE = 16 * 1024 * 1024

# Default timeouts.
CONNECT_TIMEOUT_S = 2.0
READ_TIMEOUT_S = 5.0


# ── Wire helpers ─────────────────────────────────────────────────────────

def _send_msg(sock: socket.socket, obj: dict[str, Any]) -> None:
    """Send a length-prefixed JSON message."""
    payload = json.dumps(obj, separators=(",", ":")).encode("utf-8")
    header = struct.pack("!I", len(payload))
    sock.sendall(header + payload)


def _recv_msg(sock: socket.socket) -> dict[str, Any] | None:
    """Receive a length-prefixed JSON message.  Returns None on connection error."""
    header = _recv_exact(sock, 4)
    if header is None:
        return None
    (length,) = struct.unpack("!I", header)
    if length > MAX_MSG_SIZE:
        logger.warning("message too large: %d bytes", length)
        return None
    payload = _recv_exact(sock, length)
    if payload is None:
        return None
    return json.loads(payload)


def _recv_exact(sock: socket.socket, n: int) -> bytes | None:
    """Read exactly *n* bytes from the socket."""
    buf = bytearray()
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            return None
        buf.extend(chunk)
    return bytes(buf)


# ── Serialisation helpers ────────────────────────────────────────────────

def _vote_req_to_dict(req: VoteRequest) -> dict[str, Any]:
    return {
        "rpc": "vote_req",
        "term": req.term,
        "candidate_id": req.candidate_id,
        "last_log_index": req.last_log_index,
        "last_log_term": req.last_log_term,
    }


def _vote_resp_from_dict(d: dict[str, Any]) -> VoteResponse:
    return VoteResponse(
        term=d["term"],
        vote_granted=d["vote_granted"],
        voter_id=d["voter_id"],
    )


def _append_req_to_dict(req: AppendEntriesRequest) -> dict[str, Any]:
    return {
        "rpc": "append_req",
        "term": req.term,
        "leader_id": req.leader_id,
        "prev_log_index": req.prev_log_index,
        "prev_log_term": req.prev_log_term,
        "entries": [e.to_dict() for e in req.entries],
        "leader_commit": req.leader_commit,
    }


def _append_resp_from_dict(d: dict[str, Any]) -> AppendEntriesResponse:
    return AppendEntriesResponse(
        term=d["term"],
        success=d["success"],
        match_index=d["match_index"],
        responder_id=d["responder_id"],
    )


def _vote_resp_to_dict(resp: VoteResponse) -> dict[str, Any]:
    return {
        "rpc": "vote_resp",
        "term": resp.term,
        "vote_granted": resp.vote_granted,
        "voter_id": resp.voter_id,
    }


def _append_resp_to_dict(resp: AppendEntriesResponse) -> dict[str, Any]:
    return {
        "rpc": "append_resp",
        "term": resp.term,
        "success": resp.success,
        "match_index": resp.match_index,
        "responder_id": resp.responder_id,
    }


# ── TCP Transport (client side) ─────────────────────────────────────────

class TcpRaftTransport(RaftTransport):
    """TCP-based Raft transport.

    Each peer is identified by ``"host:port"`` (e.g. ``"10.0.0.2:9400"``).
    One TCP connection is opened per RPC call (short-lived).

    Parameters
    ----------
    peer_addresses : dict[str, tuple[str, int]]
        Mapping of node_id → (host, port).
    """

    def __init__(self, peer_addresses: dict[str, tuple[str, int]] | None = None) -> None:
        self._peers: dict[str, tuple[str, int]] = dict(peer_addresses or {})

    def set_peer(self, node_id: str, host: str, port: int) -> None:
        self._peers[node_id] = (host, port)

    # ── RPC calls ────────────────────────────────────────────────────────

    def request_vote(self, target: str, req: VoteRequest) -> VoteResponse | None:
        resp = self._rpc(target, _vote_req_to_dict(req))
        if resp is not None:
            return _vote_resp_from_dict(resp)
        return None

    def append_entries(self, target: str, req: AppendEntriesRequest) -> AppendEntriesResponse | None:
        resp = self._rpc(target, _append_req_to_dict(req))
        if resp is not None:
            return _append_resp_from_dict(resp)
        return None

    def _rpc(self, target: str, msg: dict[str, Any]) -> dict[str, Any] | None:
        addr = self._peers.get(target)
        if addr is None:
            logger.warning("unknown peer: %s", target)
            return None
        try:
            with socket.create_connection(addr, timeout=CONNECT_TIMEOUT_S) as sock:
                sock.settimeout(READ_TIMEOUT_S)
                _send_msg(sock, msg)
                return _recv_msg(sock)
        except (OSError, json.JSONDecodeError) as exc:
            logger.debug("rpc to %s failed: %s", target, exc)
            return None


# ── TCP Listener (server side) ───────────────────────────────────────────

class TcpRaftListener:
    """Listens for inbound Raft RPCs and dispatches to a local RaftNode.

    Parameters
    ----------
    node : RaftNode
        The local Raft node to handle incoming RPCs.
    host : str
        Bind address.
    port : int
        Bind port.
    """

    def __init__(self, node: RaftNode, host: str = "0.0.0.0", port: int = 9400) -> None:
        self._node = node
        self._host = host
        self._port = port
        self._server_sock: socket.socket | None = None
        self._running = False
        self._thread: threading.Thread | None = None

    def start(self) -> None:
        """Start the listener in a background thread."""
        self._server_sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._server_sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._server_sock.bind((self._host, self._port))
        self._server_sock.listen(32)
        self._server_sock.settimeout(1.0)  # Allow periodic shutdown check
        self._running = True
        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()
        logger.info("raft listener started on %s:%d", self._host, self._port)

    def stop(self) -> None:
        """Stop the listener."""
        self._running = False
        if self._server_sock:
            self._server_sock.close()
        if self._thread:
            self._thread.join(timeout=3.0)

    def _accept_loop(self) -> None:
        while self._running:
            try:
                conn, addr = self._server_sock.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            threading.Thread(target=self._handle_conn, args=(conn,), daemon=True).start()

    def _handle_conn(self, conn: socket.socket) -> None:
        try:
            conn.settimeout(READ_TIMEOUT_S)
            msg = _recv_msg(conn)
            if msg is None:
                return

            rpc = msg.get("rpc")
            resp: dict[str, Any] | None = None

            if rpc == "vote_req":
                req = VoteRequest(
                    term=msg["term"],
                    candidate_id=msg["candidate_id"],
                    last_log_index=msg["last_log_index"],
                    last_log_term=msg["last_log_term"],
                )
                result = self._node.handle_vote_request(req)
                resp = _vote_resp_to_dict(result)

            elif rpc == "append_req":
                entries = [LogEntry.from_dict(e) for e in msg.get("entries", [])]
                req = AppendEntriesRequest(
                    term=msg["term"],
                    leader_id=msg["leader_id"],
                    prev_log_index=msg["prev_log_index"],
                    prev_log_term=msg["prev_log_term"],
                    entries=entries,
                    leader_commit=msg["leader_commit"],
                )
                result = self._node.handle_append_entries(req)
                resp = _append_resp_to_dict(result)
            else:
                logger.warning("unknown rpc type: %s", rpc)

            if resp is not None:
                _send_msg(conn, resp)
        except (OSError, json.JSONDecodeError) as exc:
            logger.debug("error handling connection: %s", exc)
        finally:
            conn.close()
