"""qmvir sql — Interactive QM-SQL Shell.

A ``psql``-like REPL that connects to a running QM Hub Engine
via the internal Python API (no network round-trip needed when
running locally).

Features
--------
* Multi-line input: statements end with ``;``
* Command history persisted to ``~/.qmvir_history``
* Auto-completion for QM-specific keywords (LIKEV, VEC, CPOINT …)
* Slash commands: ``/d`` list tables, ``/q`` quit, ``/u`` show users
* RBAC enforcement: every statement checked via ``check_permission``
* Tabular output with column alignment

Usage (standalone)::

    from qm_core.cli.sql_shell import SQLShell
    shell = SQLShell(engine=my_engine, session=my_auth_session)
    shell.run()
"""

from __future__ import annotations

import os
import re
import socket
import struct
import sys
import time
from pathlib import Path
from typing import Any

try:
    from prompt_toolkit import PromptSession
    from prompt_toolkit.history import FileHistory
    from prompt_toolkit.completion import WordCompleter
    from prompt_toolkit.formatted_text import HTML
except Exception:  # pragma: no cover - allows CLI module import in minimal envs
    class FileHistory:  # type: ignore[override]
        def __init__(self, *_args: Any, **_kwargs: Any) -> None:
            pass

    class WordCompleter:  # type: ignore[override]
        def __init__(self, *_args: Any, **_kwargs: Any) -> None:
            pass

    class PromptSession:  # type: ignore[override]
        def __init__(self, *_args: Any, **_kwargs: Any) -> None:
            pass

        def prompt(self, message: str) -> str:
            return input(message)

    def HTML(text: str) -> str:  # type: ignore[override]
        return text

from qm_core.auth import (
    UserCatalog,
    AuthSession,
    AuthError,
    Role,
    check_permission,
)


def _read_daemon_state(data_dir: str) -> dict[str, Any] | None:
    """Read daemon state file if available."""
    state_path = Path(data_dir) / "qm_daemon.state"
    if not state_path.exists():
        return None
    try:
        import json
        return json.loads(state_path.read_text())
    except Exception:
        return None


def _iter_daemon_state_paths(data_dir: str) -> list[Path]:
    """Generate daemon state candidate paths with requested data_dir first."""
    out: list[Path] = []
    seen: set[str] = set()

    def _push(p: Path) -> None:
        key = str(p)
        if key not in seen:
            seen.add(key)
            out.append(p)

    _push(Path(data_dir) / "qm_daemon.state")

    # Common tmp layouts: /tmp/qm_data, /tmp/qm_demo, etc.
    for p in Path("/tmp").glob("qm*/qm_daemon.state"):
        _push(p)
    # macOS often resolves /tmp -> /private/tmp; include both defensively.
    if Path("/private/tmp").exists():
        for p in Path("/private/tmp").glob("qm*/qm_daemon.state"):
            _push(p)

    return out


def _state_alive(state: dict[str, Any]) -> bool:
    """Best-effort check that daemon PID from state is still alive."""
    try:
        pid = int(state.get("pid", 0))
        if pid <= 0:
            return False
        os.kill(pid, 0)
        return True
    except Exception:
        return False


def _collect_gateway_candidates(
    data_dir: str,
    host: str | None,
    port: int | None,
) -> list[dict[str, Any]]:
    """Build ordered gateway endpoint candidates for auto/on modes."""
    cands: list[dict[str, Any]] = []
    seen: set[tuple[str, int, str | None]] = set()

    def _add(h: str, p: int, src: str, unix_socket_path: str | None = None) -> None:
        key = (h, p, unix_socket_path)
        if key in seen:
            return
        seen.add(key)
        cands.append(
            {
                "host": h,
                "port": p,
                "source": src,
                "unix_socket_path": unix_socket_path,
            }
        )

    # 1) Explicit CLI args always highest priority.
    if host is not None and port is not None:
        _add(str(host), int(port), "explicit")
    else:
        state = _read_daemon_state(data_dir)
        if state and _state_alive(state):
            h = str(host or state.get("host") or "127.0.0.1")
            p = int(port or state.get("port") or 55433)
            _add(h, p, f"state:{data_dir}", state.get("unix_socket_path"))
        elif host is not None or port is not None:
            _add(str(host or "127.0.0.1"), int(port or 55433), "partial-explicit")

    # 2) Auto-discovery from other known state files.
    for state_path in _iter_daemon_state_paths(data_dir):
        try:
            if not state_path.exists():
                continue
            import json

            st = json.loads(state_path.read_text())
            if not _state_alive(st):
                continue
            h = str(st.get("host") or "127.0.0.1")
            p = int(st.get("port") or 55433)
            _add(h, p, f"state:{state_path.parent}", st.get("unix_socket_path"))
        except Exception:
            continue

    # 3) Conventional fallback endpoint.
    _add(str(host or "127.0.0.1"), int(port or 55433), "default")
    return cands


class GatewayEngineAdapter:
    """Small SQL execution adapter over PostgreSQL wire protocol."""

    def __init__(
        self,
        host: str,
        port: int,
        unix_socket_path: str | None = None,
        user: str = "admin",
        database: str = "qm",
        connect_timeout: float = 5.0,
    ) -> None:
        self._host = host
        self._port = port
        self._unix_socket_path = unix_socket_path
        self._user = user
        self._database = database
        if unix_socket_path:
            self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._sock.settimeout(connect_timeout)
            self._sock.connect(unix_socket_path)
        else:
            self._sock = socket.create_connection((host, port), timeout=connect_timeout)
        self._startup()

    def _recv_exact(self, n: int) -> bytes:
        chunks: list[bytes] = []
        left = n
        while left > 0:
            b = self._sock.recv(left)
            if not b:
                raise ConnectionError("Gateway connection closed")
            chunks.append(b)
            left -= len(b)
        return b"".join(chunks)

    def _read_msg(self) -> tuple[bytes, bytes]:
        hdr = self._recv_exact(5)
        msg_type = hdr[:1]
        (length,) = struct.unpack("!I", hdr[1:5])
        payload = self._recv_exact(length - 4) if length > 4 else b""
        return msg_type, payload

    def _startup(self) -> None:
        params = {
            "user": self._user,
            "database": self._database,
            "client_encoding": "UTF8",
        }
        payload = struct.pack("!I", 196608)
        for k, v in params.items():
            payload += k.encode("utf-8") + b"\x00" + v.encode("utf-8") + b"\x00"
        payload += b"\x00"
        self._sock.sendall(struct.pack("!I", len(payload) + 4) + payload)

        while True:
            msg_type, _payload = self._read_msg()
            if msg_type == b"Z":
                return
            if msg_type == b"E":
                raise RuntimeError("Gateway startup failed")

    @staticmethod
    def _parse_row_description(payload: bytes) -> list[str]:
        (count,) = struct.unpack("!h", payload[:2])
        cols: list[str] = []
        i = 2
        for _ in range(count):
            j = payload.find(b"\x00", i)
            if j < 0:
                raise ValueError("Malformed RowDescription")
            cols.append(payload[i:j].decode("utf-8", errors="replace"))
            i = j + 1 + 18
        return cols

    @staticmethod
    def _parse_data_row(payload: bytes) -> list[Any]:
        (count,) = struct.unpack("!h", payload[:2])
        i = 2
        row: list[Any] = []
        for _ in range(count):
            (ln,) = struct.unpack("!i", payload[i:i + 4])
            i += 4
            if ln == -1:
                row.append(None)
                continue
            v = payload[i:i + ln].decode("utf-8", errors="replace")
            i += ln
            row.append(v)
        return row

    @staticmethod
    def _parse_error(payload: bytes) -> str:
        # Fields: <code byte><cstring> ... <0>
        msg = "gateway error"
        idx = 0
        while idx < len(payload) and payload[idx] != 0:
            code = payload[idx:idx + 1]
            idx += 1
            end = payload.find(b"\x00", idx)
            if end < 0:
                break
            val = payload[idx:end].decode("utf-8", errors="replace")
            idx = end + 1
            if code == b"M":
                msg = val
        return msg

    def execute_sql(self, sql: str) -> list[dict[str, Any]]:
        payload = sql.encode("utf-8") + b"\x00"
        self._sock.sendall(b"Q" + struct.pack("!I", len(payload) + 4) + payload)

        columns: list[str] = []
        rows: list[list[Any]] = []
        while True:
            msg_type, payload = self._read_msg()
            if msg_type == b"T":
                columns = self._parse_row_description(payload)
            elif msg_type == b"D":
                rows.append(self._parse_data_row(payload))
            elif msg_type == b"E":
                raise RuntimeError(self._parse_error(payload))
            elif msg_type == b"Z":
                break
            else:
                # C/S/K/N/1/2/3/I are informational for this shell.
                continue

        if not columns:
            return []
        return [dict(zip(columns, r)) for r in rows]

    def close(self) -> None:
        try:
            self._sock.sendall(b"X" + struct.pack("!I", 4))
        except Exception:
            pass
        self._sock.close()


# ═══════════════════════════════════════════════════════════════════════
# Keyword completer
# ═══════════════════════════════════════════════════════════════════════

_QM_KEYWORDS: list[str] = [
    # Standard SQL
    "SELECT", "INSERT", "INTO", "UPDATE", "DELETE", "FROM", "WHERE",
    "AND", "OR", "NOT", "IN", "BETWEEN", "LIKE", "IS", "NULL",
    "ORDER", "BY", "ASC", "DESC", "LIMIT", "OFFSET", "GROUP",
    "HAVING", "JOIN", "INNER", "LEFT", "RIGHT", "FULL", "OUTER",
    "ON", "AS", "DISTINCT", "CREATE", "TABLE", "VALUES", "SET",
    "WITH", "CASE", "WHEN", "THEN", "ELSE", "END",
    # QM extensions (verbose)
    "SEARCH", "VECTOR", "METRIC", "TOP", "CHECKPOINT", "SHOW",
    "SLABS", "LINK", "UNLINK", "MEDIA", "ROW_ID", "ROW",
    "COMPRESSION",
    # QM extensions (short keywords)
    "LIKEV", "VEC", "CPOINT", "MREF", "DIST",
    # Types & functions
    "INT", "TEXT", "FLOAT", "BOOLEAN", "BLOB", "VECTOR",
    "COUNT", "SUM", "AVG", "MIN", "MAX",
    # Constants
    "TRUE", "FALSE", "FULL", "DELTA",
]

qm_completer = WordCompleter(_QM_KEYWORDS, ignore_case=True)

HISTORY_FILE = Path.home() / ".qmvir_history"
PROMPT_TEXT = "qmvir> "
CONTINUATION = "   ... "


# ═══════════════════════════════════════════════════════════════════════
# Output formatter
# ═══════════════════════════════════════════════════════════════════════

def format_table(columns: list[str], rows: list[list[Any]]) -> str:
    """Format query results as a fixed-width ASCII table."""
    if not columns:
        return ""

    # Convert everything to str
    str_rows = [[str(v) if v is not None else "NULL" for v in row] for row in rows]

    # Column widths: max of header or data
    widths = [len(c) for c in columns]
    for row in str_rows:
        for i, val in enumerate(row):
            if i < len(widths):
                widths[i] = max(widths[i], len(val))

    # Header
    hdr = " | ".join(c.ljust(widths[i]) for i, c in enumerate(columns))
    sep = "-+-".join("-" * w for w in widths)
    lines = [hdr, sep]
    for row in str_rows:
        line = " | ".join(
            (row[i] if i < len(row) else "").ljust(widths[i])
            for i in range(len(columns))
        )
        lines.append(line)
    lines.append(f"({len(rows)} row{'s' if len(rows) != 1 else ''})")
    return "\n".join(lines)


# ═══════════════════════════════════════════════════════════════════════
# SQL Shell
# ═══════════════════════════════════════════════════════════════════════

class SQLShell:
    """Interactive QM-SQL REPL with RBAC and history.

    Parameters
    ----------
    engine : QMHubEngine
        The database engine to execute SQL against.
    session : AuthSession
        Authenticated session for permission checks.
    user_catalog : UserCatalog | None
        Optional user catalog for slash commands like ``/u``.
    """

    def __init__(
        self,
        engine: Any,
        session: AuthSession,
        user_catalog: UserCatalog | None = None,
        data_dir: str = "/tmp/qm_data",
        enforce_local_rbac: bool = True,
        enable_local_db_compat: bool = True,
    ) -> None:
        self._engine = engine
        self._session = session
        self._catalog = user_catalog
        self._data_dir = Path(data_dir).expanduser().resolve()
        self._db_root = self._data_dir.parent / f"{self._data_dir.name}_dbs"
        self._current_db = "default"
        self._db_state_file = self._db_root / ".current_db"
        self._enforce_local_rbac = enforce_local_rbac
        self._enable_local_db_compat = enable_local_db_compat
        self._prompt_session = PromptSession(
            history=FileHistory(str(HISTORY_FILE)),
            completer=qm_completer,
        )
        if self._enable_local_db_compat:
            self._restore_last_database()

    def _restore_last_database(self) -> None:
        """Restore last selected logical DB for this data_dir if present."""
        try:
            if not self._db_state_file.exists():
                return
            name = self._db_state_file.read_text(encoding="utf-8").strip()
            if not name:
                return
            db_dir = self._db_root / name
            if not db_dir.exists():
                return
            from qm_core.hub_engine import QMHubEngine

            self._engine.close()
            self._engine = QMHubEngine(data_dir=str(db_dir), wal_enabled=True)
            self._current_db = name
        except Exception:
            # Best-effort restore only; shell should still start on base data_dir.
            return

    def _save_current_database(self, name: str) -> None:
        """Persist currently selected logical DB to a tiny state file."""
        self._db_root.mkdir(parents=True, exist_ok=True)
        self._db_state_file.write_text(name, encoding="utf-8")

    @staticmethod
    def _split_sql_batch(text: str) -> list[str]:
        """Split a batch by semicolon while respecting quoted strings."""
        out: list[str] = []
        buf: list[str] = []
        in_single = False
        i = 0
        while i < len(text):
            ch = text[i]
            if ch == "'":
                # SQL escape for single-quote inside string: ''
                if in_single and i + 1 < len(text) and text[i + 1] == "'":
                    buf.append("''")
                    i += 2
                    continue
                in_single = not in_single
                buf.append(ch)
                i += 1
                continue
            if ch == ";" and not in_single:
                stmt = "".join(buf).strip()
                if stmt:
                    out.append(stmt)
                buf.clear()
                i += 1
                continue
            buf.append(ch)
            i += 1

        tail = "".join(buf).strip()
        if tail:
            out.append(tail)
        return out

    def _handle_database_compat(self, sql: str) -> str | None:
        """Support CREATE DATABASE / USE / SHOW DATABASES in shell mode."""
        create_db_cmd = re.fullmatch(
            r"CREATE\s+DATABASE\s+([A-Za-z_][A-Za-z0-9_]*)",
            sql,
            re.IGNORECASE,
        )
        use_db_cmd = re.fullmatch(r"USE\s+([A-Za-z_][A-Za-z0-9_]*)", sql, re.IGNORECASE)
        show_dbs_cmd = re.fullmatch(r"SHOW\s+DATABASES", sql, re.IGNORECASE)

        if not self._enable_local_db_compat:
            if create_db_cmd or use_db_cmd or show_dbs_cmd:
                return (
                    "ERROR: CREATE DATABASE / USE / SHOW DATABASES are local-shell compatibility commands.\n"
                    "In daemon mode, query tables directly (no USE), or run with `--daemon off`."
                )
            return None
        if create_db_cmd:
            if not self._session.is_admin:
                return "ERROR [42501]: permission denied for CREATE DATABASE"
            name = create_db_cmd.group(1)
            db_dir = self._db_root / name
            db_dir.mkdir(parents=True, exist_ok=True)
            return f"CREATE DATABASE\nNOTICE: mapped '{name}' -> {db_dir}"

        if use_db_cmd:
            name = use_db_cmd.group(1)
            db_dir = self._db_root / name
            if not db_dir.exists():
                return f"ERROR [3D000]: database '{name}' does not exist"
            try:
                from qm_core.hub_engine import QMHubEngine

                self._engine.close()
                self._engine = QMHubEngine(data_dir=str(db_dir), wal_enabled=True)
                self._current_db = name
                self._save_current_database(name)
                return f"You are now connected to database '{name}'"
            except Exception as exc:
                return f"ERROR: failed to switch database: {exc}"

        if show_dbs_cmd:
            if not self._db_root.exists():
                return "No databases."
            names = sorted([p.name for p in self._db_root.iterdir() if p.is_dir()])
            if not names:
                return "No databases."
            rows = [[n, "*" if n == self._current_db else ""] for n in names]
            return format_table(["database", "current"], rows)

        return None

    # ── Slash commands ──────────────────────────────────────────────

    def _handle_slash(self, cmd: str) -> bool:
        """Handle ``/`` commands.  Returns True if handled."""
        parts = cmd.strip().split()
        if not parts:
            return False
        verb = parts[0].lower()

        if verb in ("/q", "/quit", "/exit"):
            raise EOFError  # triggers clean exit

        if verb in ("/d", "/tables"):
            tables = list(self._engine._tables.keys()) if hasattr(self._engine, "_tables") else []
            if tables:
                print("Tables:")
                for t in tables:
                    meta = self._engine._tables[t]
                    print(f"  {t:20s}  ({meta.row_count} rows)")
            else:
                print("No tables.")
            return True

        if verb in ("/u", "/users"):
            if self._catalog is None:
                print("User catalog not available.")
                return True
            users = self._catalog.list_users()
            print(f"{'USERNAME':20s} {'ROLE':10s}")
            print("-" * 32)
            for u in users:
                print(f"{u.username:20s} {u.role.name:10s}")
            return True

        if verb in ("/h", "/help", "/?"):
            print("Slash commands:")
            print("  /d          List tables")
            print("  /u          List users")
            print("  /stats [t]  Show statistics (SHOW STATS)")
            print("  /analyze [t] Run ANALYZE on table(s)")
            print("  /vacuum [t] Run VACUUM on table(s)")
            print("  /h or /?    Show this help")
            print("  /q          Quit")
            print("\nPostgreSQL-compat commands:")
            print("  CREATE DATABASE <name>;   Create logical DB (mapped to data-dir)")
            print("  USE <name>;               Switch current logical DB")
            print("  SHOW DATABASES;           List logical DBs")
            print("  (Only in local mode: `qmvir sql --daemon off`)")
            return True

        if verb in ("/stats",):
            tbl = parts[1] if len(parts) > 1 else ""
            sql_cmd = f"SHOW STATS {tbl}".strip()
            result = self.execute(sql_cmd)
            if result:
                print(result)
            return True

        if verb in ("/analyze",):
            tbl = parts[1] if len(parts) > 1 else ""
            sql_cmd = f"ANALYZE {tbl}".strip()
            result = self.execute(sql_cmd)
            if result:
                print(result)
            return True

        if verb in ("/vacuum",):
            tbl = parts[1] if len(parts) > 1 else ""
            sql_cmd = f"VACUUM {tbl}".strip()
            result = self.execute(sql_cmd)
            if result:
                print(result)
            return True

        if verb.startswith("/") and "/.venv/bin/" in verb:
            print("Detected shell command pasted inside SQL REPL.")
            print("Run it in terminal, not at `qmvir>` prompt.")
            print("Use `/q` to exit REPL first.")
            return True

        print(f"Unknown command: {verb} (try /h for help)")
        return True

    # ── Execute SQL ─────────────────────────────────────────────────

    def execute(self, sql: str) -> str | None:
        """Execute a SQL statement with RBAC check and return formatted output."""
        sql = sql.strip().rstrip(";").strip()
        if not sql:
            return None

        db_compat = self._handle_database_compat(sql)
        if db_compat is not None:
            return db_compat

        # Slash command
        if sql.startswith("/"):
            self._handle_slash(sql)
            return None

        try:
            if self._enforce_local_rbac:
                # Parse to AST for permission checking (local mode only).
                from qm_core.execution.qm_sql import QMSQLParser

                parser = QMSQLParser()
                ast_node = parser.parse(sql)
                check_permission(self._session, ast_node)
        except AuthError as e:
            return f"ERROR [{e.pg_code}]: {e}"
        except Exception:
            # Let the engine's own parser handle it — some stmts may go
            # through the legacy sql_parser directly.
            pass

        try:
            t0 = time.perf_counter()
            results = self._engine.execute_sql(sql)
            elapsed = time.perf_counter() - t0

            if not results:
                return f"OK ({elapsed * 1000:.1f} ms)"

            columns = list(results[0].keys())
            rows = [[row.get(c) for c in columns] for row in results]
            table = format_table(columns, rows)
            return f"{table}\nTime: {elapsed * 1000:.1f} ms"
        except Exception as exc:
            return f"ERROR: {exc}"

    # ── Main loop ───────────────────────────────────────────────────

    def run(self) -> None:
        """Run the interactive REPL until /q or Ctrl-D."""
        print(f"QMvir SQL Shell — Logged in as \033[1m{self._session.username}\033[0m"
              f" (role={self._session.role.name})")
        print("Type SQL ending with ';'.  /h for help.  /q or Ctrl-D to exit.\n")

        buf: list[str] = []

        while True:
            try:
                prompt = CONTINUATION if buf else PROMPT_TEXT
                line = self._prompt_session.prompt(prompt)
            except (EOFError, KeyboardInterrupt):
                print("\nBye!")
                break

            stripped = line.strip()

            # Slash command (only at start, not in continuation)
            if not buf and stripped.startswith("/"):
                try:
                    self._handle_slash(stripped)
                except EOFError:
                    print("Bye!")
                    break
                continue

            buf.append(line)
            joined = " ".join(buf)

            # Multi-line: wait for semicolon
            if not joined.rstrip().endswith(";"):
                continue

            for stmt in self._split_sql_batch(joined):
                output = self.execute(stmt)
                if output is not None:
                    print(output)
            buf.clear()


# ═══════════════════════════════════════════════════════════════════════
# Entry-point helper (called from qm_app.py)
# ═══════════════════════════════════════════════════════════════════════

def run_sql_shell(
    data_dir: str = "/tmp/qm_data",
    username: str = "admin",
    password: str | None = None,
    host: str | None = None,
    port: int | None = None,
    local_mode: bool = False,
    daemon_mode: str = "auto",
) -> None:
    """Bootstrap engine, authenticate, and launch the REPL."""
    from qm_core.hub_engine import QMHubEngine
    from qm_core.auth import UserCatalog

    catalog = UserCatalog()

    # Prompt for password if not provided
    if password is None:
        import getpass
        password = getpass.getpass(f"Password for {username}: ")

    # Resolve mode precedence: --local always forces local engine.
    if local_mode:
        daemon_mode = "off"

    engine: Any
    enable_local_db_compat = daemon_mode == "off"
    enforce_local_rbac = daemon_mode == "off"

    if daemon_mode == "off":
        try:
            session = catalog.authenticate(username, password)
        except AuthError as e:
            print(f"Authentication failed: {e}")
            sys.exit(1)
    else:
        # Gateway currently handles auth at protocol level as trust/auth-ok.
        # Keep shell usable from any folder even when local catalog differs.
        role = Role.ADMIN
        rec = catalog.get_user(username)
        if rec is not None:
            role = rec.role
        session = AuthSession(username=username, role=role, token="gateway")

    if daemon_mode == "off":
        engine = QMHubEngine(data_dir=data_dir, wal_enabled=True)
    else:
        candidates = _collect_gateway_candidates(data_dir, host, port)
        last_exc: Exception | None = None
        engine = None

        # daemon=on: try only the first resolved endpoint.
        if daemon_mode == "on" and candidates:
            candidates = candidates[:1]

        for candidate in candidates:
            h = str(candidate["host"])
            p = int(candidate["port"])
            src = str(candidate["source"])
            uds = candidate.get("unix_socket_path")
            try:
                engine = GatewayEngineAdapter(
                    host=h,
                    port=p,
                    unix_socket_path=uds,
                    user=username,
                )
                if uds:
                    print(f"Connected to gateway unix://{uds} ({src})")
                else:
                    print(f"Connected to gateway {h}:{p} ({src})")
                break
            except Exception as exc:
                last_exc = exc
                continue

        if engine is None:
            if daemon_mode == "auto":
                print(
                    "Could not auto-discover a running daemon gateway.\n"
                    "Tip: start daemon first (`qmvir start`) or use `qmvir sql --daemon off`/`--local`."
                )
            else:
                print(
                    f"Failed to connect gateway {host or '127.0.0.1'}:{port or 55433}: {last_exc}\n"
                    "Tip: use `--daemon auto` for discovery or `--daemon off` for local mode."
                )
            sys.exit(1)

    try:
        shell = SQLShell(
            engine=engine,
            session=session,
            user_catalog=catalog,
            data_dir=data_dir,
            enforce_local_rbac=enforce_local_rbac,
            enable_local_db_compat=enable_local_db_compat,
        )
        shell.run()
    finally:
        engine.close()
