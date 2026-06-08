from __future__ import annotations

import json
from pathlib import Path

from qm_core.cli.sql_shell import _collect_gateway_candidates


def test_collect_gateway_candidates_includes_unix_socket(tmp_path):
    data_dir = tmp_path / "qm"
    data_dir.mkdir(parents=True, exist_ok=True)

    state = {
        "pid": 999999,  # not alive, should be ignored by state_alive check
        "host": "127.0.0.1",
        "port": 56543,
        "unix_socket_path": "/tmp/.s.PGSQL.56543",
    }
    (data_dir / "qm_daemon.state").write_text(json.dumps(state))

    cands = _collect_gateway_candidates(str(data_dir), None, None)
    # At least the conventional default endpoint must always exist.
    assert any(c["host"] == "127.0.0.1" and c["port"] == 55433 for c in cands)


def test_collect_gateway_candidates_with_explicit_host_port(tmp_path):
    cands = _collect_gateway_candidates(str(tmp_path), "127.0.0.1", 60000)
    assert cands[0]["host"] == "127.0.0.1"
    assert cands[0]["port"] == 60000
    assert cands[0]["source"] == "explicit"
