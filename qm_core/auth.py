"""QM Auth — Token-based RBAC integrated into the Hub.

Provides a lightweight user/role system baked directly into the
database kernel.  No external auth server required.

Roles
-----
    ADMIN  — full access (DDL, CPOINT, SLABS, GRANT/REVOKE, all DML)
    WRITER — mutating DML (INSERT, UPDATE, DELETE, LINK/UNLINK)
    READER — read-only access (SELECT, LIKEV, MREF, DIST)

User Catalog
------------
The catalog is a dict persisted alongside checkpoints.  Passwords
are stored as ``scrypt`` hashes (N=2**14, r=8, p=1).  A bootstrap
``admin`` account is created on first startup.

Wire-protocol Integration
-------------------------
On ``StartupMessage`` the server extracts ``user`` + expects a
cleartext or MD5 password message.  ``authenticate()`` returns
an ``AuthSession`` or raises ``AuthError`` with PG error code
``28P01`` (invalid_password).

SQL ACL
-------
``check_permission(role, stmt_node)`` maps AST node types to the
minimum role required.  If the role is insufficient an
``AuthError(code='42501')`` is raised (insufficient_privilege).
"""

from __future__ import annotations

import hashlib
import os
import secrets
from dataclasses import dataclass, field
from enum import IntEnum
from typing import Any


# ═══════════════════════════════════════════════════════════════════════
# Roles
# ═══════════════════════════════════════════════════════════════════════

class Role(IntEnum):
    """Ordered privilege levels — higher value ⊃ lower permissions."""
    READER = 1
    WRITER = 2
    ADMIN  = 3


# ═══════════════════════════════════════════════════════════════════════
# Error
# ═══════════════════════════════════════════════════════════════════════

class AuthError(Exception):
    """Raised on authentication / authorization failure.

    Attributes
    ----------
    pg_code : str
        SQLSTATE error code compatible with the PG wire protocol.
    """

    def __init__(self, message: str, pg_code: str = "28P01") -> None:
        super().__init__(message)
        self.pg_code = pg_code


# ═══════════════════════════════════════════════════════════════════════
# Password hashing — scrypt based
# ═══════════════════════════════════════════════════════════════════════

_SCRYPT_N = 2 ** 14   # CPU/memory cost
_SCRYPT_R = 8
_SCRYPT_P = 1
_KEY_LEN  = 32
_SALT_LEN = 16


def _hash_password(password: str, salt: bytes | None = None) -> tuple[bytes, bytes]:
    """Return ``(salt, derived_key)`` using scrypt."""
    if salt is None:
        salt = os.urandom(_SALT_LEN)
    dk = hashlib.scrypt(
        password.encode("utf-8"),
        salt=salt,
        n=_SCRYPT_N,
        r=_SCRYPT_R,
        p=_SCRYPT_P,
        dklen=_KEY_LEN,
    )
    return salt, dk


def _verify_password(password: str, salt: bytes, stored_key: bytes) -> bool:
    """Constant-time comparison of scrypt-derived key."""
    _, dk = _hash_password(password, salt=salt)
    return secrets.compare_digest(dk, stored_key)


# ═══════════════════════════════════════════════════════════════════════
# User record
# ═══════════════════════════════════════════════════════════════════════

@dataclass(slots=True)
class UserRecord:
    """One entry in the system user catalog."""
    username: str
    salt: bytes
    password_hash: bytes
    role: Role

    def to_dict(self) -> dict[str, Any]:
        return {
            "username": self.username,
            "salt": self.salt.hex(),
            "password_hash": self.password_hash.hex(),
            "role": self.role.name,
        }

    @classmethod
    def from_dict(cls, d: dict[str, Any]) -> UserRecord:
        return cls(
            username=d["username"],
            salt=bytes.fromhex(d["salt"]),
            password_hash=bytes.fromhex(d["password_hash"]),
            role=Role[d["role"]],
        )


# ═══════════════════════════════════════════════════════════════════════
# Auth session (returned after successful login)
# ═══════════════════════════════════════════════════════════════════════

@dataclass(frozen=True, slots=True)
class AuthSession:
    """Immutable token representing an authenticated user."""
    username: str
    role: Role
    token: str          # random hex token for session tracking

    @property
    def is_admin(self) -> bool:
        return self.role >= Role.ADMIN

    @property
    def can_write(self) -> bool:
        return self.role >= Role.WRITER

    @property
    def can_read(self) -> bool:
        return self.role >= Role.READER


# ═══════════════════════════════════════════════════════════════════════
# User Catalog (Hub system table)
# ═══════════════════════════════════════════════════════════════════════

_DEFAULT_ADMIN_USER = "admin"
_DEFAULT_ADMIN_PASS = "admin"     # overridden by env QM_ADMIN_PASSWORD


class UserCatalog:
    """In-memory user catalog that can be serialized for checkpointing.

    On first creation a bootstrap ``admin`` account is generated.
    The bootstrap password can be overridden via the
    ``QM_ADMIN_PASSWORD`` environment variable.
    """

    def __init__(self) -> None:
        self._users: dict[str, UserRecord] = {}
        self._bootstrap()

    # ── Bootstrap ───────────────────────────────────────────────────

    def _bootstrap(self) -> None:
        boot_pass = os.environ.get("QM_ADMIN_PASSWORD", _DEFAULT_ADMIN_PASS)
        if _DEFAULT_ADMIN_USER not in self._users:
            self.create_user(_DEFAULT_ADMIN_USER, boot_pass, Role.ADMIN)

    # ── CRUD ────────────────────────────────────────────────────────

    def create_user(self, username: str, password: str, role: Role) -> UserRecord:
        if username in self._users:
            raise AuthError(f"User '{username}' already exists", pg_code="42710")
        salt, dk = _hash_password(password)
        rec = UserRecord(username=username, salt=salt, password_hash=dk, role=role)
        self._users[username] = rec
        return rec

    def drop_user(self, username: str) -> None:
        if username not in self._users:
            raise AuthError(f"User '{username}' does not exist", pg_code="42704")
        if username == _DEFAULT_ADMIN_USER and len(self._users) == 1:
            raise AuthError("Cannot drop the last admin user", pg_code="42501")
        del self._users[username]

    def alter_role(self, username: str, new_role: Role) -> None:
        rec = self._users.get(username)
        if rec is None:
            raise AuthError(f"User '{username}' does not exist", pg_code="42704")
        self._users[username] = UserRecord(
            username=rec.username,
            salt=rec.salt,
            password_hash=rec.password_hash,
            role=new_role,
        )

    def get_user(self, username: str) -> UserRecord | None:
        return self._users.get(username)

    def list_users(self) -> list[UserRecord]:
        return list(self._users.values())

    # ── Authentication ──────────────────────────────────────────────

    def authenticate(self, username: str, password: str) -> AuthSession:
        """Validate credentials and return an ``AuthSession``.

        Raises ``AuthError(pg_code='28P01')`` on failure.
        """
        rec = self._users.get(username)
        if rec is None or not _verify_password(password, rec.salt, rec.password_hash):
            raise AuthError("Invalid username or password", pg_code="28P01")
        token = secrets.token_hex(16)
        return AuthSession(username=rec.username, role=rec.role, token=token)

    # ── Serialization (for checkpoint) ──────────────────────────────

    def to_dict(self) -> list[dict[str, Any]]:
        return [u.to_dict() for u in self._users.values()]

    def load_from_dict(self, data: list[dict[str, Any]]) -> None:
        self._users.clear()
        for d in data:
            rec = UserRecord.from_dict(d)
            self._users[rec.username] = rec


# ═══════════════════════════════════════════════════════════════════════
# SQL ACL — map AST node → minimum role
# ═══════════════════════════════════════════════════════════════════════

# Import names only — avoid circular imports at module level by
# using strings; actual resolution happens in check_permission().

_ADMIN_STMTS = frozenset({
    "CheckpointStmt",
    "ShowSlabsStmt",
    "SetCompressionStmt",
    "CreateTableStmt",
    "MrefStmt",
})

_WRITER_STMTS = frozenset({
    "InsertStmt",
    "UpdateStmt",
    "DeleteStmt",
    "LinkMediaStmt",
    "UnlinkMediaStmt",
})

_READER_STMTS = frozenset({
    "SelectStmt",
    "SearchVectorStmt",
    "DistColumn",
})


def check_permission(session: AuthSession, ast_node: Any) -> None:
    """Raise ``AuthError(pg_code='42501')`` if the session's role is
    insufficient for the given AST statement node.

    Admin can do everything.  Permission is checked by AST class name
    so that ``qm_core.auth`` never imports the parser module directly
    (avoids circular deps).
    """
    cls_name = type(ast_node).__name__

    if cls_name in _ADMIN_STMTS:
        required = Role.ADMIN
    elif cls_name in _WRITER_STMTS:
        required = Role.WRITER
    elif cls_name in _READER_STMTS:
        required = Role.READER
    else:
        # Unknown / extension statements default to ADMIN
        required = Role.ADMIN

    if session.role < required:
        raise AuthError(
            f"Permission denied: role {session.role.name} cannot execute "
            f"{cls_name} (requires {required.name})",
            pg_code="42501",
        )
