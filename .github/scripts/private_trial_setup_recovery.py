"""Offline recovery decisions for the dormant private-trial setup rehearsal.

The planner consumes redacted, normalized readbacks. It never connects to a
database or provider and it never issues a write. A caller can use the
decision to resume the reviewed apply, skip a create whose result is proven,
or hold the checkpoint for reconciliation.
"""

from dataclasses import dataclass
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


@dataclass(frozen=True)
class ConnectionReadback:
    config_id: str
    account_id: str
    request_ref: str
    binding: str
    database: tuple[str, int]
    role: tuple[str, int]
    origin: str
    access_client_ref: str
    origin_ca_ref: str
    sslmode: str
    caching_disabled: bool


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
    return value


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
    return value


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
    pre = observation.catalog_digest == intent.pre_apply_digest and not observation.ledger
    post = (observation.catalog_digest == intent.post_apply_digest
            and observation.ledger == intent.expected_ledger)
    if post:
        return ApplyDecision("skip_apply", "complete post-apply state is present")
    if pre:
        return ApplyDecision("apply", "complete pre-apply state is present")
    return ApplyDecision("hold", "catalog or ledger state is incomplete or mismatched")


def _connection_fields(intent: ConnectionIntent):
    expected = (intent.account_id, intent.request_ref, intent.binding, intent.database,
                intent.role, intent.origin, intent.access_client_ref, intent.origin_ca_ref,
                intent.sslmode, intent.caching_disabled)
    return expected


def decide_lost_create(intent: ConnectionIntent,
                       readbacks: Iterable[ConnectionReadback]) -> CreateDecision:
    """Resolve a lost connection-create acknowledgement from scoped evidence."""
    expected = _connection_fields(intent)
    for field, value in zip(
        ("account", "request reference", "binding", "origin", "access client", "origin CA"),
        expected[:3] + expected[5:8], strict=True,
    ):
        _text(value, field)
    _text(intent.sslmode, "sslmode")
    if intent.sslmode != "verify-full" or intent.caching_disabled is not True:
        return CreateDecision("hold", "connection settings are outside the reviewed contract")
    matches = []
    for row in readbacks:
        values = (row.account_id, row.request_ref, row.binding, row.database, row.role,
                  row.origin, row.access_client_ref, row.origin_ca_ref,
                  row.sslmode, row.caching_disabled)
        if values == expected:
            if not CONFIG_ID.fullmatch(row.config_id):
                raise RecoveryInputError("invalid config ID")
            matches.append(row.config_id)
    if len(matches) != 1:
        return CreateDecision("hold", "create result is missing, ambiguous or out of scope")
    return CreateDecision("skip_create", "one scoped result is bound to the original request", matches[0])


def plan_cleanup(scope: CleanupScope,
                 readbacks: Iterable[ResourceReadback]) -> CleanupDecision:
    """Return only exact, attempt-owned resources for a cleanup preview."""
    _text(scope.attempt_id, "attempt")
    _text(scope.account_id, "account")
    if not scope.resource_ids or len(set(scope.resource_ids)) != len(scope.resource_ids):
        raise RecoveryInputError("invalid cleanup resource IDs")
    selected = set(scope.resource_ids)
    matches = {}
    for row in readbacks:
        _text(row.resource_id, "resource ID")
        _text(row.attempt_id, "resource attempt")
        _text(row.account_id, "resource account")
        _text(row.kind, "resource kind")
        if row.resource_id in selected and row.attempt_id == scope.attempt_id and row.account_id == scope.account_id:
            if row.resource_id in matches:
                return CleanupDecision("hold", "cleanup readback repeats a selected resource")
            matches[row.resource_id] = row
    if set(matches) != selected:
        return CleanupDecision("hold", "cleanup scope lacks an exact readback")
    return CleanupDecision("cleanup", "all selected resources are exact attempt-owned matches",
                          tuple(scope.resource_ids))
