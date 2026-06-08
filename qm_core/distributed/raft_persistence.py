"""QM Distributed — Persistent Raft State.

Provides durable storage for the three pieces of Raft persistent state:
    1. currentTerm
    2. votedFor
    3. log[]

Uses atomic write (write-to-temp → fsync → rename) for the metadata file
and an append-only file for log entries.

Directory layout::

    <state_dir>/
        meta.json        — {term, voted_for}
        raft.log         — newline-delimited JSON, one entry per line
"""

from __future__ import annotations

import json
import logging
import os
import tempfile
from pathlib import Path
from typing import Any

from qm_core.distributed.consensus import LogEntry, LogEntryType

logger = logging.getLogger(__name__)


class PersistentRaftState:
    """Persistent storage for Raft node state.

    Thread-safety: callers must hold the Raft node lock before calling
    mutating methods.  Reads are safe without a lock (atomic rename
    guarantees consistent snapshots of meta.json).
    """

    def __init__(self, state_dir: str | Path) -> None:
        self._dir = Path(state_dir)
        self._dir.mkdir(parents=True, exist_ok=True)
        self._meta_path = self._dir / "meta.json"
        self._log_path = self._dir / "raft.log"

        # Load from disk
        self._term: int = 0
        self._voted_for: str | None = None
        self._log: list[LogEntry] = []
        self._load()

    # ── Public read API ──────────────────────────────────────────────────

    @property
    def current_term(self) -> int:
        return self._term

    @property
    def voted_for(self) -> str | None:
        return self._voted_for

    @property
    def log(self) -> list[LogEntry]:
        return self._log

    def last_log_index(self) -> int:
        return self._log[-1].index if self._log else 0

    def last_log_term(self) -> int:
        return self._log[-1].term if self._log else 0

    # ── Public write API ─────────────────────────────────────────────────

    def save_term_and_vote(self, term: int, voted_for: str | None) -> None:
        """Persist currentTerm and votedFor (atomic write)."""
        self._term = term
        self._voted_for = voted_for
        self._write_meta()

    def append_entries(self, entries: list[LogEntry]) -> None:
        """Append new log entries (append-only)."""
        if not entries:
            return
        with open(self._log_path, "a", encoding="utf-8") as f:
            for entry in entries:
                line = json.dumps(entry.to_dict(), separators=(",", ":"))
                f.write(line + "\n")
            f.flush()
            os.fsync(f.fileno())
        self._log.extend(entries)

    def truncate_log(self, from_index: int) -> None:
        """Remove all log entries with index >= from_index and rewrite log file."""
        self._log = [e for e in self._log if e.index < from_index]
        self._rewrite_log()

    def compact(self, through_index: int) -> None:
        """Remove entries up to *through_index* (log compaction / snapshotting stub).

        In a full implementation this would write a snapshot file and then
        truncate the prefix.  For now we just drop old entries.
        """
        self._log = [e for e in self._log if e.index > through_index]
        self._rewrite_log()

    # ── Internal persistence ─────────────────────────────────────────────

    def _write_meta(self) -> None:
        """Atomically write meta.json (temp → fsync → rename)."""
        data = json.dumps(
            {"term": self._term, "voted_for": self._voted_for},
            separators=(",", ":"),
        ).encode("utf-8")

        fd, tmp_path = tempfile.mkstemp(dir=self._dir, suffix=".tmp")
        try:
            os.write(fd, data)
            os.fsync(fd)
            os.close(fd)
            os.replace(tmp_path, self._meta_path)
        except BaseException:
            os.close(fd) if not os.get_inheritable(fd) else None
            if os.path.exists(tmp_path):
                os.unlink(tmp_path)
            raise

    def _rewrite_log(self) -> None:
        """Rewrite the entire log file (after truncation)."""
        fd, tmp_path = tempfile.mkstemp(dir=self._dir, suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                for entry in self._log:
                    f.write(json.dumps(entry.to_dict(), separators=(",", ":")) + "\n")
                f.flush()
                os.fsync(f.fileno())
            os.replace(tmp_path, str(self._log_path))
        except BaseException:
            if os.path.exists(tmp_path):
                os.unlink(tmp_path)
            raise

    def _load(self) -> None:
        """Load state from disk on startup."""
        # Meta
        if self._meta_path.exists():
            try:
                raw = self._meta_path.read_text(encoding="utf-8")
                meta = json.loads(raw)
                self._term = meta.get("term", 0)
                self._voted_for = meta.get("voted_for")
            except (json.JSONDecodeError, OSError) as exc:
                logger.warning("failed to load raft meta: %s", exc)

        # Log
        if self._log_path.exists():
            try:
                with open(self._log_path, "r", encoding="utf-8") as f:
                    for line_no, line in enumerate(f, 1):
                        line = line.strip()
                        if not line:
                            continue
                        try:
                            d = json.loads(line)
                            self._log.append(LogEntry.from_dict(d))
                        except (json.JSONDecodeError, KeyError) as exc:
                            logger.warning("corrupt log entry at line %d: %s", line_no, exc)
            except OSError as exc:
                logger.warning("failed to load raft log: %s", exc)
