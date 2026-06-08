from __future__ import annotations

from qm_core.engine import QMEngine
from qm_core.hub_engine import QMHubEngine


def test_engine_vector_gate_requires_suggestion_context() -> None:
    assert QMEngine._allow_vector_layer({"purpose": "suggestion"}) is True
    assert QMEngine._allow_vector_layer({"allow_vector_join": True}) is True
    assert QMEngine._allow_vector_layer({"purpose": "analytics"}) is False


def test_hub_vector_gate_requires_suggestion_context() -> None:
    assert QMHubEngine._allow_vector_layer({"purpose": "recommendation"}) is True
    assert QMHubEngine._allow_vector_layer({"allow_vector_join": True}) is True
    assert QMHubEngine._allow_vector_layer({"purpose": "sql"}) is False
