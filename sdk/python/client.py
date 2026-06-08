"""QM SDK — Python client for the QM Data Gateway.

Usage:
    from sdk.python.client import QMClient

    client = QMClient("http://localhost:8400", api_key="...")

    # CRUD
    articles = client.find("articles", where={"status": "published"}, limit=10)

    # Search
    results = client.search("articles", text="database performance", strategy="hybrid")

    # Analytics
    stats = client.aggregate("events", group_by=["event_type"], metrics=[{"count": "*"}])
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


@dataclass
class QMResponse:
    """Response from the QM gateway."""

    ok: bool
    data: Any = None
    error: str | None = None
    meta: dict[str, Any] | None = None


class QMClient:
    """Python client for the QM Data Gateway."""

    def __init__(
        self,
        base_url: str = "http://localhost:8400",
        api_key: str | None = None,
        tenant_id: str | None = None,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.tenant_id = tenant_id

    def find(
        self,
        entity: str,
        where: dict[str, Any] | None = None,
        select: list[str] | None = None,
        order_by: list[dict[str, str]] | None = None,
        limit: int = 20,
        offset: int = 0,
    ) -> QMResponse:
        """Find entities by filter."""
        payload: dict[str, Any] = {
            "action": "find",
            "entity": entity,
            "limit": limit,
            "offset": offset,
        }
        if where:
            payload["where"] = where
        if select:
            payload["select"] = select
        if order_by:
            payload["order_by"] = order_by

        return self._request(payload)

    def get(self, entity: str, pk: str) -> QMResponse:
        """Get a single entity by primary key."""
        return self._request({
            "action": "get",
            "entity": entity,
            "where": {"id": pk},
            "limit": 1,
        })

    def insert(self, entity: str, data: dict[str, Any]) -> QMResponse:
        """Insert a new entity."""
        return self._request({
            "action": "insert",
            "entity": entity,
            "data": data,
        })

    def update(
        self, entity: str, pk: str, data: dict[str, Any]
    ) -> QMResponse:
        """Update an entity."""
        return self._request({
            "action": "update",
            "entity": entity,
            "where": {"id": pk},
            "data": data,
        })

    def delete(self, entity: str, pk: str) -> QMResponse:
        """Delete an entity."""
        return self._request({
            "action": "delete",
            "entity": entity,
            "where": {"id": pk},
        })

    def search(
        self,
        collection: str,
        text: str,
        filters: dict[str, Any] | None = None,
        strategy: str = "lexical",
        limit: int = 10,
    ) -> QMResponse:
        """Search a collection."""
        strategy_map = {
            "lexical": {"lexical": True},
            "vector": {"vector": True},
            "hybrid": {"lexical": True, "vector": True},
            "rerank": {"lexical": True, "vector": True, "rerank": True},
        }
        return self._request({
            "action": "search",
            "collection": collection,
            "text": text,
            "filters": filters,
            "strategy": strategy_map.get(strategy, {"lexical": True}),
            "limit": limit,
        })

    def aggregate(
        self,
        dataset: str,
        group_by: list[str],
        metrics: list[dict[str, str]],
        where: dict[str, Any] | None = None,
    ) -> QMResponse:
        """Run an aggregation query."""
        payload: dict[str, Any] = {
            "action": "aggregate",
            "dataset": dataset,
            "group_by": group_by,
            "metrics": metrics,
        }
        if where:
            payload["where"] = where
        return self._request(payload)

    def _request(self, payload: dict[str, Any]) -> QMResponse:
        """Send a request to the gateway. Uses aiohttp/httpx in production."""
        try:
            import json
            import urllib.request

            headers = {"Content-Type": "application/json"}
            if self.api_key:
                headers["Authorization"] = f"Bearer {self.api_key}"
            if self.tenant_id:
                headers["X-Tenant-ID"] = self.tenant_id

            data = json.dumps(payload).encode("utf-8")
            req = urllib.request.Request(
                f"{self.base_url}/query",
                data=data,
                headers=headers,
                method="POST",
            )
            with urllib.request.urlopen(req) as resp:
                body = json.loads(resp.read())
                return QMResponse(
                    ok=body.get("ok", False),
                    data=body.get("data"),
                    error=body.get("error"),
                    meta=body.get("meta"),
                )
        except Exception as exc:
            return QMResponse(ok=False, error=str(exc))
