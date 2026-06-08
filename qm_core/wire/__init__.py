"""QM Wire — PostgreSQL Wire Protocol (Frontend/Backend).

Implements the PostgreSQL v3 wire protocol so that standard
Postgres drivers (psycopg2, JDBC, Go pq, node-postgres) can
connect to QM as if it were a real PostgreSQL server.

Message format:
    - Startup: Int32(len) + Int32(protocol=196608) + params
    - Regular: Byte1(type) + Int32(len) + payload

Supported message flow:
    Client                          Server (QM)
    ──────                          ────────────
    StartupMessage          →
                            ←       AuthOK (R)
                            ←       ParameterStatus (S) ×N
                            ←       BackendKeyData (K)
                            ←       ReadyForQuery (Z)
    Query (Q)               →
                            ←       RowDescription (T)
                            ←       DataRow (D) ×N
                            ←       CommandComplete (C)
                            ←       ReadyForQuery (Z)
    Terminate (X)           →
"""

from __future__ import annotations

import struct
from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


# ── Message Types ───────────────────────────────────────────────────

class FrontendMsg(bytes, Enum):
    """Messages from client → server."""
    QUERY        = b'Q'
    PARSE        = b'P'
    BIND         = b'B'
    DESCRIBE     = b'D'
    EXECUTE      = b'E'
    SYNC         = b'S'
    FLUSH        = b'H'
    CLOSE        = b'C'
    TERMINATE    = b'X'
    PASSWORD     = b'p'
    COPY_DATA    = b'd'
    COPY_DONE    = b'c'
    COPY_FAIL    = b'f'


class BackendMsg(bytes, Enum):
    """Messages from server → client."""
    AUTH_REQUEST      = b'R'
    PARAMETER_STATUS  = b'S'
    BACKEND_KEY_DATA  = b'K'
    READY_FOR_QUERY   = b'Z'
    ROW_DESCRIPTION   = b'T'
    DATA_ROW          = b'D'
    COMMAND_COMPLETE  = b'C'
    ERROR_RESPONSE    = b'E'
    NOTICE_RESPONSE   = b'N'
    EMPTY_QUERY       = b'I'
    PARSE_COMPLETE    = b'1'
    BIND_COMPLETE     = b'2'
    CLOSE_COMPLETE    = b'3'
    NO_DATA           = b'n'


class TxnStatus(bytes, Enum):
    """Transaction status for ReadyForQuery."""
    IDLE         = b'I'
    IN_TXN       = b'T'
    FAILED       = b'E'


# ── Wire format helpers ─────────────────────────────────────────────

def _pack_string(s: str) -> bytes:
    """Null-terminated C string."""
    return s.encode("utf-8") + b"\x00"


def _pack_msg(msg_type: bytes, payload: bytes) -> bytes:
    """Pack a backend message: type(1) + len(4) + payload."""
    length = 4 + len(payload)  # length includes itself
    return msg_type + struct.pack("!I", length) + payload


def _pack_int16(n: int) -> bytes:
    return struct.pack("!h", n)


def _pack_int32(n: int) -> bytes:
    return struct.pack("!i", n)


# ── Protocol Codec ──────────────────────────────────────────────────

class PgProtocol:
    """PostgreSQL wire protocol message builder and parser.

    This is a stateless codec — it builds and parses individual messages.
    The session state machine wraps this to manage connection lifecycle.
    """

    # ── Handshake messages ──────────────────────────────────────────

    @staticmethod
    def parse_startup(data: bytes) -> dict[str, str]:
        """Parse a StartupMessage → dict of parameters.

        Format: Int32(length) + Int32(protocol_version) + key\0val\0...key\0val\0\0
        """
        # Already stripped the length prefix by caller
        protocol = struct.unpack("!I", data[:4])[0]
        # Protocol 3.0 = 196608 (0x30000)
        params: dict[str, str] = {"_protocol_version": str(protocol)}

        # Parse null-terminated key-value pairs
        rest = data[4:]
        parts = rest.split(b"\x00")
        i = 0
        while i + 1 < len(parts) and parts[i]:
            key = parts[i].decode("utf-8", errors="replace")
            val = parts[i + 1].decode("utf-8", errors="replace")
            params[key] = val
            i += 2

        return params

    @staticmethod
    def build_auth_ok() -> bytes:
        """AuthenticationOk (R) — no password required."""
        return _pack_msg(b"R", _pack_int32(0))

    @staticmethod
    def build_parameter_status(key: str, value: str) -> bytes:
        """ParameterStatus (S) — server parameter."""
        return _pack_msg(b"S", _pack_string(key) + _pack_string(value))

    @staticmethod
    def build_backend_key_data(pid: int, secret: int) -> bytes:
        """BackendKeyData (K)."""
        return _pack_msg(b"K", _pack_int32(pid) + _pack_int32(secret))

    @staticmethod
    def build_ready_for_query(status: TxnStatus = TxnStatus.IDLE) -> bytes:
        """ReadyForQuery (Z)."""
        return _pack_msg(b"Z", status.value)

    # ── Query response messages ─────────────────────────────────────

    @staticmethod
    def build_row_description(columns: list[tuple[str, int]]) -> bytes:
        """RowDescription (T).

        Parameters
        ----------
        columns : list of (name, oid)
            Column name and type OID.
            Common OIDs: 23=int4, 25=text, 701=float8, 1043=varchar
        """
        parts: list[bytes] = []
        parts.append(_pack_int16(len(columns)))
        for name, oid in columns:
            parts.append(_pack_string(name))
            parts.append(_pack_int32(0))    # table OID
            parts.append(_pack_int16(0))    # column number
            parts.append(_pack_int32(oid))  # type OID
            parts.append(_pack_int16(-1))   # type size (-1 = variable)
            parts.append(_pack_int32(-1))   # type modifier
            parts.append(_pack_int16(0))    # format (0=text)
        return _pack_msg(b"T", b"".join(parts))

    @staticmethod
    def build_data_row(values: list[Optional[str]]) -> bytes:
        """DataRow (D) — one row of text-format values."""
        parts: list[bytes] = [_pack_int16(len(values))]
        for val in values:
            if val is None:
                parts.append(_pack_int32(-1))  # NULL
            else:
                encoded = val.encode("utf-8")
                parts.append(_pack_int32(len(encoded)))
                parts.append(encoded)
        return _pack_msg(b"D", b"".join(parts))

    @staticmethod
    def build_command_complete(tag: str) -> bytes:
        """CommandComplete (C) — e.g. 'SELECT 5', 'INSERT 0 1'."""
        return _pack_msg(b"C", _pack_string(tag))

    @staticmethod
    def build_error_response(
        severity: str = "ERROR",
        code: str = "42601",
        message: str = "syntax error",
    ) -> bytes:
        """ErrorResponse (E)."""
        parts: list[bytes] = []
        parts.append(b"S" + _pack_string(severity))     # Severity
        parts.append(b"V" + _pack_string(severity))     # Severity (non-localized)
        parts.append(b"C" + _pack_string(code))         # SQLSTATE code
        parts.append(b"M" + _pack_string(message))      # Message
        parts.append(b"\x00")                            # Terminator
        return _pack_msg(b"E", b"".join(parts))

    @staticmethod
    def build_empty_query() -> bytes:
        """EmptyQueryResponse (I)."""
        return _pack_msg(b"I", b"")

    @staticmethod
    def build_parse_complete() -> bytes:
        """ParseComplete (1)."""
        return _pack_msg(b"1", b"")

    @staticmethod
    def build_bind_complete() -> bytes:
        """BindComplete (2)."""
        return _pack_msg(b"2", b"")

    @staticmethod
    def build_close_complete() -> bytes:
        """CloseComplete (3)."""
        return _pack_msg(b"3", b"")

    @staticmethod
    def build_no_data() -> bytes:
        """NoData (n)."""
        return _pack_msg(b"n", b"")

    @staticmethod
    def build_parameter_description(param_oids: list[int]) -> bytes:
        """ParameterDescription (t)."""
        payload = bytearray()
        payload.extend(_pack_int16(len(param_oids)))
        for oid in param_oids:
            payload.extend(_pack_int32(int(oid)))
        return _pack_msg(b"t", bytes(payload))

    # ── Frontend message parsing ────────────────────────────────────

    @staticmethod
    def parse_query(data: bytes) -> str:
        """Parse a Query (Q) message → SQL string."""
        # data = null-terminated SQL string
        return data.rstrip(b"\x00").decode("utf-8", errors="replace")

    @staticmethod
    def read_message(data: bytes) -> tuple[bytes, int, bytes]:
        """Read one frontend message from a byte buffer.

        Returns (msg_type, total_consumed_bytes, payload).
        """
        if len(data) < 5:
            raise ValueError("Incomplete message header")
        msg_type = data[0:1]
        (length,) = struct.unpack("!I", data[1:5])
        total = 1 + length  # type byte + length (which includes itself)
        if len(data) < total:
            raise ValueError(f"Incomplete message: need {total}, have {len(data)}")
        payload = data[5:total]
        return msg_type, total, payload


# ── Type OID constants (subset of PostgreSQL) ───────────────────────

class PgTypeOID:
    BOOL     = 16
    INT2     = 21
    INT4     = 23
    INT8     = 20
    FLOAT4   = 700
    FLOAT8   = 701
    TEXT     = 25
    VARCHAR  = 1043
    BYTEA    = 17
    JSON     = 114
    JSONB    = 3802
    TIMESTAMP = 1114
    NUMERIC  = 1700

    @classmethod
    def from_qm_type(cls, qm_type: str) -> int:
        """Map QM DataType name to PostgreSQL OID."""
        mapping = {
            "int": cls.INT4, "integer": cls.INT4,
            "bigint": cls.INT8,
            "float": cls.FLOAT4, "double": cls.FLOAT8,
            "text": cls.TEXT, "varchar": cls.VARCHAR,
            "boolean": cls.BOOL, "bool": cls.BOOL,
            "blob": cls.BYTEA,
            "json": cls.JSONB,
            "timestamp": cls.TIMESTAMP,
            "vector": cls.FLOAT4,  # vectors represented as float4[]
        }
        return mapping.get(qm_type.lower(), cls.TEXT)


# ── Connection session ──────────────────────────────────────────────

class PgSession:
    """Manages one PostgreSQL wire protocol connection.

    Handles the startup handshake and simple query protocol.
    Delegates actual SQL execution to a callback.
    """

    def __init__(self, execute_fn=None, pid: int = 1, secret: int = 0):
        """
        Parameters
        ----------
        execute_fn : callable(sql: str) -> tuple[list[str], list[list[Any]]]
            Callback that takes SQL string and returns (column_names, rows).
            Each row is a list of values (str or None).
        """
        self._execute_fn = execute_fn
        self._pid = pid
        self._secret = secret
        self._params: dict[str, str] = {}
        self._in_txn = False
        self._prepared: dict[str, tuple[str, list[int]]] = {}
        self._portals: dict[str, tuple[str, list[str], list[int]]] = {}

    def handle_startup(self, data: bytes) -> bytes:
        """Process StartupMessage, return handshake response bytes."""
        self._params = PgProtocol.parse_startup(data)

        response = bytearray()
        response.extend(PgProtocol.build_auth_ok())

        # Report server parameters
        server_params = {
            "server_version": "16.0 (QM)",
            "server_encoding": "UTF8",
            "client_encoding": "UTF8",
            "DateStyle": "ISO, MDY",
            "integer_datetimes": "on",
            "standard_conforming_strings": "on",
            # Capability hint so clients/tools can detect QM-native extensions.
            "qm_features": "vector_search,media_link,mref,slabs,checkpoint",
        }
        for k, v in server_params.items():
            response.extend(PgProtocol.build_parameter_status(k, v))

        response.extend(PgProtocol.build_backend_key_data(self._pid, self._secret))
        response.extend(PgProtocol.build_ready_for_query())
        return bytes(response)

    def handle_query(self, sql: str) -> bytes:
        """Process a simple Query message, return response bytes."""
        response = bytearray()

        if not sql.strip():
            response.extend(PgProtocol.build_empty_query())
            response.extend(PgProtocol.build_ready_for_query())
            return bytes(response)

        try:
            if self._execute_fn is None:
                raise RuntimeError("No execute function configured")

            exec_result = self._execute_fn(sql)

            # Backward compatible executor contract:
            # - (columns, rows)
            # - (columns, column_oids, rows)
            if isinstance(exec_result, tuple) and len(exec_result) == 3:
                columns, column_oids, rows = exec_result
            else:
                columns, rows = exec_result  # type: ignore[misc]
                column_oids = [PgTypeOID.TEXT] * len(columns)

            if columns:
                # Build RowDescription
                col_defs = [
                    (
                        name,
                        int(column_oids[i]) if i < len(column_oids) else PgTypeOID.TEXT,
                    )
                    for i, name in enumerate(columns)
                ]
                response.extend(PgProtocol.build_row_description(col_defs))

                # Build DataRows
                for row in rows:
                    str_vals = [str(v) if v is not None else None for v in row]
                    response.extend(PgProtocol.build_data_row(str_vals))

                # Determine tag
                sql_upper = sql.strip().upper()
                if sql_upper.startswith("SELECT"):
                    tag = f"SELECT {len(rows)}"
                elif sql_upper.startswith("INSERT"):
                    tag = f"INSERT 0 {len(rows)}"
                elif sql_upper.startswith("UPDATE"):
                    tag = f"UPDATE {len(rows)}"
                elif sql_upper.startswith("DELETE"):
                    tag = f"DELETE {len(rows)}"
                else:
                    tag = "OK"
            else:
                tag = "OK"

            response.extend(PgProtocol.build_command_complete(tag))

        except Exception as exc:
            response.extend(PgProtocol.build_error_response(
                message=str(exc),
            ))

        status = TxnStatus.IN_TXN if self._in_txn else TxnStatus.IDLE
        response.extend(PgProtocol.build_ready_for_query(status))
        return bytes(response)

    def _execute_sql(self, sql: str) -> tuple[list[str], list[int], list[list[object]]]:
        if self._execute_fn is None:
            raise RuntimeError("No execute function configured")
        exec_result = self._execute_fn(sql)
        if isinstance(exec_result, tuple) and len(exec_result) == 3:
            columns, column_oids, rows = exec_result
            return columns, [int(x) for x in column_oids], rows
        columns, rows = exec_result  # type: ignore[misc]
        return columns, [PgTypeOID.TEXT] * len(columns), rows

    @staticmethod
    def _read_cstring(payload: bytes, pos: int) -> tuple[str, int]:
        end = payload.find(b"\x00", pos)
        if end < 0:
            raise ValueError("Malformed message: missing cstring terminator")
        return payload[pos:end].decode("utf-8", errors="replace"), end + 1

    @staticmethod
    def _decode_param_value(raw: Optional[bytes], fmt: int) -> str:
        if raw is None:
            return "NULL"
        if fmt == 1:
            # Binary format. Handle common fixed-width numerics; fallback to text.
            if len(raw) == 4:
                return str(struct.unpack("!i", raw)[0])
            if len(raw) == 8:
                return str(struct.unpack("!q", raw)[0])
            try:
                return raw.decode("utf-8")
            except Exception:
                return "'\\x" + raw.hex() + "'"

        text_val = raw.decode("utf-8", errors="replace")
        if text_val.upper() == "NULL":
            return "NULL"
        is_number = False
        try:
            float(text_val)
            is_number = True
        except Exception:
            is_number = False
        if is_number:
            return text_val
        return "'" + text_val.replace("'", "''") + "'"

    @staticmethod
    def _substitute_params(sql: str, decoded_params: list[str]) -> str:
        out = sql
        for i, val in enumerate(decoded_params, start=1):
            out = out.replace(f"${i}", val)
        return out

    def _handle_parse(self, payload: bytes) -> bytes:
        pos = 0
        stmt_name, pos = self._read_cstring(payload, pos)
        query, pos = self._read_cstring(payload, pos)
        (num_types,) = struct.unpack("!h", payload[pos:pos + 2])
        pos += 2
        param_oids: list[int] = []
        for _ in range(max(0, num_types)):
            (oid,) = struct.unpack("!i", payload[pos:pos + 4])
            pos += 4
            param_oids.append(oid)
        self._prepared[stmt_name] = (query, param_oids)
        return PgProtocol.build_parse_complete()

    def _handle_bind(self, payload: bytes) -> bytes:
        pos = 0
        portal, pos = self._read_cstring(payload, pos)
        statement, pos = self._read_cstring(payload, pos)

        query, _param_oids = self._prepared.get(statement, ("", []))
        if not query:
            raise ValueError(f"Unknown prepared statement: {statement!r}")

        (num_format_codes,) = struct.unpack("!h", payload[pos:pos + 2])
        pos += 2
        format_codes: list[int] = []
        for _ in range(max(0, num_format_codes)):
            (fmt,) = struct.unpack("!h", payload[pos:pos + 2])
            pos += 2
            format_codes.append(fmt)

        (num_params,) = struct.unpack("!h", payload[pos:pos + 2])
        pos += 2
        param_bytes: list[Optional[bytes]] = []
        for _ in range(max(0, num_params)):
            (length,) = struct.unpack("!i", payload[pos:pos + 4])
            pos += 4
            if length < 0:
                param_bytes.append(None)
            else:
                param_bytes.append(payload[pos:pos + length])
                pos += length

        # Skip result-column format codes (not used, always return text rows).
        if pos + 2 <= len(payload):
            (num_result_formats,) = struct.unpack("!h", payload[pos:pos + 2])
            pos += 2
            pos += max(0, num_result_formats) * 2

        decoded: list[str] = []
        for idx, raw in enumerate(param_bytes):
            if not format_codes:
                fmt = 0
            elif len(format_codes) == 1:
                fmt = format_codes[0]
            else:
                fmt = format_codes[idx] if idx < len(format_codes) else 0
            decoded.append(self._decode_param_value(raw, fmt))

        sql = self._substitute_params(query, decoded)
        self._portals[portal] = (sql, [], [])
        return PgProtocol.build_bind_complete()

    def _handle_describe(self, payload: bytes) -> bytes:
        if not payload:
            return PgProtocol.build_no_data()
        kind = payload[0:1]
        name = payload[1:].rstrip(b"\x00").decode("utf-8", errors="replace")
        response = bytearray()

        if kind == b"S":
            query, param_oids = self._prepared.get(name, ("", []))
            response.extend(PgProtocol.build_parameter_description(param_oids))
            if not query:
                response.extend(PgProtocol.build_no_data())
                return bytes(response)
            # Try a safe describe path by replacing params with NULL.
            safe_sql = query
            for i in range(1, len(param_oids) + 1):
                safe_sql = safe_sql.replace(f"${i}", "NULL")
            try:
                columns, oids, _rows = self._execute_sql(safe_sql)
                if columns:
                    response.extend(PgProtocol.build_row_description(list(zip(columns, oids))))
                else:
                    response.extend(PgProtocol.build_no_data())
            except Exception:
                response.extend(PgProtocol.build_no_data())
            return bytes(response)

        if kind == b"P":
            sql, cached_cols, cached_oids = self._portals.get(name, ("", [], []))
            if not sql:
                return PgProtocol.build_no_data()
            if not cached_cols:
                try:
                    cols, oids, _rows = self._execute_sql(sql)
                    cached_cols = cols
                    cached_oids = oids
                    self._portals[name] = (sql, cached_cols, cached_oids)
                except Exception:
                    return PgProtocol.build_no_data()
            if cached_cols:
                return PgProtocol.build_row_description(list(zip(cached_cols, cached_oids)))
            return PgProtocol.build_no_data()

        return PgProtocol.build_no_data()

    def _handle_execute(self, payload: bytes) -> bytes:
        pos = 0
        portal, pos = self._read_cstring(payload, pos)
        # max_rows is accepted but ignored (send all rows).
        _max_rows = 0
        if pos + 4 <= len(payload):
            (_max_rows,) = struct.unpack("!i", payload[pos:pos + 4])

        sql, cached_cols, cached_oids = self._portals.get(portal, ("", [], []))
        if not sql:
            raise ValueError(f"Unknown portal: {portal!r}")

        had_cached_description = bool(cached_cols)
        columns, column_oids, rows = self._execute_sql(sql)
        if not cached_cols and columns:
            cached_cols = columns
            cached_oids = column_oids
            self._portals[portal] = (sql, cached_cols, cached_oids)

        response = bytearray()
        # If Describe was skipped by client, include RowDescription here.
        if columns and not had_cached_description:
            response.extend(PgProtocol.build_row_description(list(zip(columns, column_oids))))
        for row in rows:
            str_vals = [str(v) if v is not None else None for v in row]
            response.extend(PgProtocol.build_data_row(str_vals))

        sql_upper = sql.strip().upper()
        if sql_upper.startswith("SELECT"):
            tag = f"SELECT {len(rows)}"
        elif sql_upper.startswith("INSERT"):
            tag = "INSERT 0 1"
        elif sql_upper.startswith("UPDATE"):
            tag = f"UPDATE {len(rows)}"
        elif sql_upper.startswith("DELETE"):
            tag = f"DELETE {len(rows)}"
        else:
            tag = "OK"
        response.extend(PgProtocol.build_command_complete(tag))
        return bytes(response)

    def _handle_close(self, payload: bytes) -> bytes:
        if not payload:
            return PgProtocol.build_close_complete()
        kind = payload[0:1]
        name = payload[1:].rstrip(b"\x00").decode("utf-8", errors="replace")
        if kind == b"S":
            self._prepared.pop(name, None)
        elif kind == b"P":
            self._portals.pop(name, None)
        return PgProtocol.build_close_complete()

    def handle_message(self, msg_type: bytes, payload: bytes) -> bytes:
        """Route a frontend message to the appropriate handler."""
        try:
            if msg_type == FrontendMsg.QUERY:
                sql = PgProtocol.parse_query(payload)
                return self.handle_query(sql)
            if msg_type == FrontendMsg.PARSE:
                return self._handle_parse(payload)
            if msg_type == FrontendMsg.BIND:
                return self._handle_bind(payload)
            if msg_type == FrontendMsg.DESCRIBE:
                return self._handle_describe(payload)
            if msg_type == FrontendMsg.EXECUTE:
                return self._handle_execute(payload)
            if msg_type == FrontendMsg.CLOSE:
                return self._handle_close(payload)
            if msg_type == FrontendMsg.SYNC:
                return PgProtocol.build_ready_for_query(
                    TxnStatus.IN_TXN if self._in_txn else TxnStatus.IDLE
                )
            if msg_type == FrontendMsg.FLUSH:
                return b""
            if msg_type == FrontendMsg.TERMINATE:
                return b""  # close connection

            return PgProtocol.build_error_response(
                message=f"Unsupported message type: {msg_type!r}",
            )
        except Exception as exc:
            return PgProtocol.build_error_response(message=str(exc))
