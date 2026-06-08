"""Minimal asyncio PostgreSQL v3 simple-query server."""

from __future__ import annotations

import asyncio
import os
import struct
from collections.abc import Callable
from typing import Any

SSL_REQUEST_CODE = 80877103
PROTOCOL_VERSION_3 = 196608
TEXT_OID = 25


def _message(kind: bytes, payload: bytes = b"") -> bytes:
    return kind + struct.pack("!I", len(payload) + 4) + payload


def _auth_ok() -> bytes:
    return _message(b"R", struct.pack("!I", 0))


def _parameter_status(name: str, value: str) -> bytes:
    return _message(b"S", name.encode() + b"\x00" + value.encode() + b"\x00")


def _backend_key_data() -> bytes:
    return _message(b"K", struct.pack("!II", os.getpid() & 0x7FFFFFFF, 0))


def _ready() -> bytes:
    return _message(b"Z", b"I")


def _empty_query() -> bytes:
    return _message(b"I")


def _error_response(message: str) -> bytes:
    payload = b"SERROR\x00" + b"M" + message.encode("utf-8", "replace") + b"\x00\x00"
    return _message(b"E", payload)


def _row_description(columns: list[str]) -> bytes:
    payload = bytearray(struct.pack("!H", len(columns)))
    for col in columns:
        payload.extend(col.encode("utf-8"))
        payload.append(0)
        payload.extend(struct.pack("!IhIhih", 0, 0, TEXT_OID, -1, -1, 0))
    return _message(b"T", bytes(payload))


def _data_row(row: list[Any]) -> bytes:
    payload = bytearray(struct.pack("!H", len(row)))
    for value in row:
        if value is None:
            payload.extend(struct.pack("!i", -1))
        else:
            raw = str(value).encode("utf-8")
            payload.extend(struct.pack("!I", len(raw)))
            payload.extend(raw)
    return _message(b"D", bytes(payload))


def _command_complete(tag: str) -> bytes:
    return _message(b"C", tag.encode("ascii", "replace") + b"\x00")


class QMPostgresServer:
    """Small PostgreSQL-compatible server for simple-query integration tests."""

    def __init__(
        self,
        executor: Callable[[str], tuple[list[str], list[list[Any]]]],
        host: str = "127.0.0.1",
        port: int = 55433,
    ) -> None:
        self.executor = executor
        self.host = host
        self.port = port
        self._server: asyncio.AbstractServer | None = None

    @property
    def is_running(self) -> bool:
        return self._server is not None and self._server.is_serving()

    async def start(self) -> None:
        if self._server is not None:
            return
        self._server = await asyncio.start_server(
            self._handle_client,
            self.host,
            self.port,
            reuse_address=True,
        )
        await self._server.start_serving()

    async def stop(self) -> None:
        server = self._server
        self._server = None
        if server is None:
            return
        server.close()
        await server.wait_closed()

    async def _read_startup(self, reader: asyncio.StreamReader) -> None:
        while True:
            header = await reader.readexactly(4)
            (length,) = struct.unpack("!I", header)
            payload = await reader.readexactly(length - 4)
            if length == 8:
                (code,) = struct.unpack("!I", payload)
                if code == SSL_REQUEST_CODE:
                    raise _SslRequest
            if len(payload) >= 4:
                (protocol,) = struct.unpack("!I", payload[:4])
                if protocol == PROTOCOL_VERSION_3:
                    return

    async def _handle_client(
        self,
        reader: asyncio.StreamReader,
        writer: asyncio.StreamWriter,
    ) -> None:
        try:
            while True:
                try:
                    await self._read_startup(reader)
                    break
                except _SslRequest:
                    writer.write(b"N")
                    await writer.drain()

            writer.write(
                _auth_ok()
                + _parameter_status("server_version", "15.0-qmvir")
                + _parameter_status("client_encoding", "UTF8")
                + _backend_key_data()
                + _ready()
            )
            await writer.drain()

            while True:
                kind = await reader.readexactly(1)
                (length,) = struct.unpack("!I", await reader.readexactly(4))
                payload = await reader.readexactly(length - 4)
                if kind == b"X":
                    return
                if kind != b"Q":
                    writer.write(_error_response("unsupported message") + _ready())
                    await writer.drain()
                    continue

                sql = payload.rstrip(b"\x00").decode("utf-8", "replace").strip()
                if not sql:
                    writer.write(_empty_query() + _ready())
                    await writer.drain()
                    continue

                try:
                    columns, rows = self.executor(sql)
                    tag = f"SELECT {len(rows)}" if sql.upper().startswith("SELECT") else "OK"
                    response = _row_description(list(columns))
                    response += b"".join(_data_row(list(row)) for row in rows)
                    response += _command_complete(tag) + _ready()
                except Exception as exc:
                    response = _error_response(str(exc)) + _ready()
                writer.write(response)
                await writer.drain()
        except (asyncio.IncompleteReadError, ConnectionResetError):
            return
        finally:
            writer.close()
            try:
                await writer.wait_closed()
            except Exception:
                pass


class _SslRequest(Exception):
    pass


async def run_server(
    executor: Callable[[str], tuple[list[str], list[list[Any]]]],
    host: str = "127.0.0.1",
    port: int = 55433,
) -> QMPostgresServer:
    server = QMPostgresServer(executor=executor, host=host, port=port)
    await server.start()
    return server
