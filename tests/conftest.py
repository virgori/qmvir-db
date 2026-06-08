"""Shared test fixtures for QM."""

from __future__ import annotations

import pytest


@pytest.fixture
def sample_schema() -> dict:
    """Minimal entity schema for testing."""
    return {
        "name": "articles",
        "columns": [
            {"name": "id", "type": "uuid", "primary_key": True},
            {"name": "title", "type": "text"},
            {"name": "body", "type": "text"},
            {"name": "status", "type": "text"},
            {"name": "score", "type": "float"},
        ],
    }


@pytest.fixture
def sample_rows() -> list[dict]:
    """A handful of article rows for testing."""
    return [
        {"id": "a1", "title": "Intro to databases", "body": "SQL and storage engines.", "status": "published", "score": 4.5},
        {"id": "a2", "title": "Vector search guide", "body": "Embeddings and ANN.", "status": "draft", "score": 3.8},
        {"id": "a3", "title": "Cache invalidation", "body": "TTL and version-aware caching.", "status": "published", "score": 4.9},
        {"id": "a4", "title": "BM25 ranking", "body": "Term frequency and inverse document frequency.", "status": "published", "score": 4.2},
        {"id": "a5", "title": "MVCC transactions", "body": "Snapshot isolation and write conflicts.", "status": "archived", "score": 3.1},
    ]
