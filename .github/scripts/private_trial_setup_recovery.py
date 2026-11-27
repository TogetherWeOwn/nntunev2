"""Offline recovery decisions from redacted private-trial readbacks."""

from dataclasses import dataclass, field
import re
from typing import Iterable


SHA256 = re.compile(r"^[0-9a-f]{64}$")
CONFIG_ID = re.compile(r"^[0-9a-f]{32}$")


class RecoveryInputError(ValueError):
    """The offline record is malformed or outside the reviewed scope."""


@dataclass(frozen=True)
class ApplyIntent:
    attempt_id: str
    database_id: str
    migration_sha256: str
    pre_apply_digest: str
    post_apply_digest: str
    expected_ledger: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class ApplyObservation:
    catalog_digest: str
    ledger: tuple[tuple[str, str], ...]


@dataclass(frozen=True)
class ApplyDecision:
    action: str
    reason: str


@dataclass(frozen=True)
class ConnectionIntent:
    account_id: str
    request_ref: str
    binding: str
    database: tuple[str, int]
    role: tuple[str, int]
    origin: str
    access_client_ref: str
    origin_ca_ref: str
    sslmode: str = "verify-full"
    caching_disabled: bool = True


@dataclass(frozen=True, kw_only=True)
class ConnectionReadback(ConnectionIntent):
    config_id: str
    sslmode: str = field()
    caching_disabled: bool = field()


@dataclass(frozen=True)
class CreateDecision:
    action: str
    reason: str
    config_id: str | None = None


@dataclass(frozen=True)
class CleanupScope:
    attempt_id: str
    account_id: str
    resource_ids: tuple[str, ...]


@dataclass(frozen=True)
class ResourceReadback:
    resource_id: str
    attempt_id: str
    account_id: str
    kind: str


@dataclass(frozen=True)
class CleanupDecision:
    action: str
    reason: str
    resource_ids: tuple[str, ...] = ()


def _text(value, field):
    if not isinstance(value, str) or not value or any(ord(char) < 0x20 for char in value):
        raise RecoveryInputError(f"invalid {field}")
    return value


def _digest(value, field):
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise RecoveryInputError(f"invalid {field}")


def _ledger(value):
    if not isinstance(value, tuple) or any(
        not isinstance(row, tuple) or len(row) != 2
        or not isinstance(row[0], str) or not row[0]
        or not isinstance(row[1], str) or not SHA256.fullmatch(row[1])
        for row in value
    ):
        raise RecoveryInputError("invalid migration ledger")
    if len({row[0] for row in value}) != len(value):
        raise RecoveryInputError("duplicate migration ledger row")


def _check_apply(intent: ApplyIntent, observation: ApplyObservation):
    _text(intent.attempt_id, "attempt")
    _text(intent.database_id, "database")
    _digest(intent.migration_sha256, "migration digest")
    _digest(intent.pre_apply_digest, "pre-apply digest")
    _digest(intent.post_apply_digest, "post-apply digest")
    _ledger(intent.expected_ledger)
    if not intent.expected_ledger or intent.expected_ledger[-1][1] != intent.migration_sha256:
        raise RecoveryInputError("migration ledger does not identify the applied file")
    _digest(observation.catalog_digest, "catalog digest")
    _ledger(observation.ledger)


def decide_apply_recovery(intent: ApplyIntent, observation: ApplyObservation,
                          status: str) -> ApplyDecision:
    """Choose one safe next action after an interrupted catalog apply.

    A sent operation may be retried only after a complete pre-apply readback.
    A complete post-apply readback skips the write. Any mixed catalog and
    ledger state remains pending for reviewed reconciliation.
    """
    if status not in ("unsent", "sent_unknown"):
        raise RecoveryInputError("invalid apply status")
    _check_apply(intent, observation)
    pre = observation.catalog_digest == intent.pre_apply_digest and observation.ledger == intent.expected_ledger[:-1]
    post = (observation.catalog_digest == intent.post_apply_digest
            and observation.ledger == intent.expected_ledger)
    if post and status == "sent_unknown":
        return ApplyDecision("skip_apply", "complete post-apply state is present")
    if pre:
        return ApplyDecision("apply", "complete pre-apply state is present")
    return ApplyDecision("hold", "catalog or ledger state is incomplete or mismatched")


def _connection_fields(connection: ConnectionIntent):
    return (connection.account_id, connection.request_ref, connection.binding,
            connection.database, connection.role, connection.origin,
            connection.access_client_ref, connection.origin_ca_ref, connection.sslmode)


def _check_sql_identity(identity, field):
    if (not isinstance(identity, tuple) or len(identity) != 2
            or type(identity[1]) is not int or not 0 < identity[1] <= 0xFFFFFFFF):
        raise RecoveryInputError(f"invalid {field}")
    _text(identity[0], field)


def decide_lost_create(intent: ConnectionIntent,
                       readbacks: Iterable[ConnectionReadback]) -> CreateDecision:
    """Resolve a lost connection-create acknowledgement from scoped evidence."""
    expected = _connection_fields(intent)
    for field, value in zip(
        ("account", "request reference", "binding", "origin", "access client", "origin CA"),
        expected[:3] + expected[5:8], strict=True,
    ):
        _text(value, field)
    _check_sql_identity(intent.database, "database")
    _check_sql_identity(intent.role, "role")
    _text(intent.sslmode, "sslmode")
    if intent.sslmode != "verify-full" or intent.caching_disabled is not True:
        return CreateDecision("hold", "connection settings are outside the reviewed contract")
    pending = CreateDecision("hold", "create result is missing, ambiguous or out of scope")
    rows = tuple(readbacks)
    candidates = [row for row in rows if row.request_ref == intent.request_ref]
    if len(candidates) != 1:
        return pending
    row, = candidates
    if _connection_fields(row) != expected or row.caching_disabled is not True:
        return pending
    _check_sql_identity(row.database, "database")
    _check_sql_identity(row.role, "role")
    if not isinstance(row.config_id, str) or not CONFIG_ID.fullmatch(row.config_id):
        raise RecoveryInputError("invalid config ID")
    if sum(other.config_id == row.config_id for other in rows) != 1:
        return pending
    return CreateDecision("skip_create", "one scoped result is bound to the original request", row.config_id)


def plan_cleanup(scope: CleanupScope,
                 readbacks: Iterable[ResourceReadback]) -> CleanupDecision:
    """Return only exact, attempt-owned resources for a cleanup preview."""
    _text(scope.attempt_id, "attempt")
    _text(scope.account_id, "account")
    if not isinstance(scope.resource_ids, tuple) or not scope.resource_ids:
        raise RecoveryInputError("invalid cleanup resource IDs")
    selected = {_text(resource_id, "cleanup resource IDs") for resource_id in scope.resource_ids}
    if len(selected) != len(scope.resource_ids):
        raise RecoveryInputError("invalid cleanup resource IDs")
    matches = set()
    for row in readbacks:
        _text(row.resource_id, "resource ID")
        _text(row.attempt_id, "resource attempt")
        _text(row.account_id, "resource account")
        _text(row.kind, "resource kind")
        if row.resource_id not in selected:
            continue
        if row.resource_id in matches:
            return CleanupDecision("hold", "cleanup readback repeats a selected resource")
        if row.attempt_id != scope.attempt_id or row.account_id != scope.account_id:
            return CleanupDecision("hold", "cleanup readback conflicts with the selected scope")
        matches.add(row.resource_id)
    if matches != selected:
        return CleanupDecision("hold", "cleanup scope lacks an exact readback")
    return CleanupDecision("cleanup", "all selected resources are exact attempt-owned matches",
                          tuple(scope.resource_ids))
