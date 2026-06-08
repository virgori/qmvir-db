"""Authentication and access-control helpers for the Python gateway layer."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum


class Permission(Enum):
    READ = "read"
    WRITE = "write"
    DELETE = "delete"
    SEARCH = "search"
    ANALYTICS = "analytics"
    ADMIN = "admin"


@dataclass
class AuthContext:
    user_id: str
    tenant_id: str
    roles: list[str] = field(default_factory=list)
    permissions: set[Permission] = field(default_factory=set)
    api_key: str | None = None
    is_service_account: bool = False

    @property
    def is_admin(self) -> bool:
        return Permission.ADMIN in self.permissions or "admin" in self.roles


_ACTION_PERMISSIONS: dict[str, Permission] = {
    "find": Permission.READ,
    "get": Permission.READ,
    "list": Permission.READ,
    "insert": Permission.WRITE,
    "update": Permission.WRITE,
    "upsert": Permission.WRITE,
    "delete": Permission.DELETE,
    "search": Permission.SEARCH,
    "aggregate": Permission.ANALYTICS,
}


def authorize_request(ctx: AuthContext, action: str, resource: str | None = None) -> bool:
    del resource
    if ctx.is_admin:
        return True
    required = _ACTION_PERMISSIONS.get(action)
    return required is not None and required in ctx.permissions


@dataclass
class TenantIsolation:
    tenant_id: str

    def apply_filter(self, where: dict | None) -> dict:
        return {**(where or {}), "tenant_id": self.tenant_id}


class APIKeyValidator:
    def __init__(self) -> None:
        self._keys: dict[str, AuthContext] = {}

    def register_key(self, key: str, ctx: AuthContext) -> None:
        self._keys[key] = ctx

    def validate(self, key: str) -> AuthContext | None:
        return self._keys.get(key)
