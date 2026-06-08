"""QM Procedure Satellite — PL/QM stored procedure execution.

Runs PL/QM scripts in an isolated satellite process. The procedure
satellite has its own ProcedureCatalog and PLQMInterpreter. It
receives CALL commands through the ring buffer and returns results.

Architecture:
    Hub → ring.publish(DDL, "_plqm", {name, args})
    ProcedureSatellite → catalog.call(name, args) → result
    ProcedureSatellite → ring.complete(result)

The interpreter runs in the satellite's memory space — no shared
Python objects with the Hub.
"""

from __future__ import annotations

import msgpack

from qm_core.ipc.ring_buffer import CommandType
from qm_core.hub.lsn_sequencer import LSNStamp
from qm_core.satellite.base import Satellite, SatelliteConfig
from qm_core.procedures.plqm import PLQMInterpreter, PLQMError
from qm_core.procedures.catalog import ProcedureCatalog, StoredProcedure, ProcedureParam


class ProcedureSatellite(Satellite):
    """Satellite for executing PL/QM stored procedures.

    Each ProcedureSatellite maintains its own ProcedureCatalog and
    PLQMInterpreter in isolated memory space.
    """

    def __init__(self, config: SatelliteConfig, ring) -> None:
        super().__init__(config, ring)
        self._catalog = ProcedureCatalog()

    @property
    def catalog(self) -> ProcedureCatalog:
        return self._catalog

    def _execute_command(
        self,
        stamp: LSNStamp,
        cmd: CommandType,
        payload: bytes,
    ) -> bytes:
        """Execute a procedure-related command."""
        try:
            msg = msgpack.unpackb(payload, raw=False)
        except Exception:
            return msgpack.packb({"error": "invalid payload"})

        action = msg.get("action", "call")

        if action == "register":
            return self._do_register(msg)
        elif action == "drop":
            return self._do_drop(msg)
        elif action == "call" or "name" in msg:
            return self._do_call(msg, stamp)
        elif action == "list":
            return self._do_list()
        else:
            return msgpack.packb({"error": f"unknown action: {action}"})

    def _do_register(self, msg: dict) -> bytes:
        """Register a stored procedure."""
        params = []
        for p in msg.get("params", []):
            params.append(ProcedureParam(
                name=p["name"],
                type=p.get("type", "ANY"),
                default=p.get("default"),
                required=p.get("required", True),
            ))

        proc = StoredProcedure(
            name=msg["name"],
            params=params,
            body=msg["body"],
            language=msg.get("language", "plqm"),
            owner=msg.get("owner", "system"),
            description=msg.get("description", ""),
        )
        self._catalog.register(proc)
        return msgpack.packb({"registered": msg["name"]})

    def _do_drop(self, msg: dict) -> bytes:
        """Drop a stored procedure."""
        name = msg.get("name", "")
        success = self._catalog.drop(name)
        return msgpack.packb({"dropped": name, "success": success})

    def _do_call(self, msg: dict, stamp: LSNStamp) -> bytes:
        """Call a stored procedure."""
        name = msg.get("name", "")
        args = msg.get("args", {})
        try:
            result = self._catalog.call(name, args)
            return msgpack.packb({
                "result": result,
                "lsn": stamp.lsn,
                "success": True,
            })
        except (PLQMError, KeyError, ValueError) as e:
            return msgpack.packb({
                "error": str(e),
                "lsn": stamp.lsn,
                "success": False,
            })

    def _do_list(self) -> bytes:
        """List all stored procedures."""
        procs = self._catalog.list_procedures()
        return msgpack.packb({
            "procedures": [
                {"name": p.name, "params": len(p.params), "language": p.language}
                for p in procs
            ]
        })
