"""Read-only policy for the protected executor's disposable DNS record."""

from collections.abc import Callable, Sequence
from dataclasses import dataclass
from datetime import datetime
from enum import Enum


POLL_SECONDS = 2
QUALIFICATION_SECONDS = 90
DELETE_CONTAINMENT_SECONDS = 300


class Change(Enum):
    CREATE = "create"
    DELETE = "delete"


class Verdict(Enum):
    PASSED = "passed"
    STOPPED = "stopped"
    EXPIRED = "expired"
    CONTAINED = "contained after deadline"


@dataclass(frozen=True)
class Readback:
    """The executor's two REST checks for one exact change and their original time."""

    change: Change
    record_id: str
    name: str
    exact_id: bool
    exact_name: bool
    at: float


@dataclass(frozen=True)
class Observation:
    server: str
    name: str
    rcode: str
    authoritative: bool
    answer_count: int
    exact_a_count: int
    utc: datetime
    at: float


@dataclass(frozen=True)
class Result:
    """An unresolved delete requires retaining the writer for reconciliation."""

    verdict: Verdict
    reason: str
    retain_writer: bool


def check_convergence(
    change: Change,
    *,
    readback: Readback,
    record_id: str,
    name: str,
    servers: tuple[str, str],
    read_pair: Callable[[tuple[str, str], str, float], Sequence[Observation]],
    monotonic: Callable[[], float],
    sleep: Callable[[float], None],
    record: Callable[[Observation, float], None],
) -> Result:
    """Evaluate readback and DNS under the executor's writer lock.

    The adapter validates each packet against its query and pinned server,
    counts exact-owner IN A answers, and returns an observation per server
    within the round timeout. The caller persists receipts in private.
    """
    if not isinstance(change, Change):
        return Result(Verdict.STOPPED, "unknown DNS change", True)
    deleting = change is Change.DELETE

    def finish(verdict: Verdict, reason: str) -> Result:
        return Result(verdict, reason, deleting and verdict is not Verdict.PASSED)

    if (
        not readback.exact_id or not readback.exact_name
        or (readback.change, readback.record_id, readback.name) != (change, record_id, name)
    ):
        return finish(Verdict.STOPPED, "missing or mismatched REST readback")
    identities = {server.rstrip(".").lower() for server in servers}
    if not record_id or not name or len(servers) != 2 or len(identities) != 2 or "" in identities:
        return finish(Verdict.STOPPED, "exact record and two distinct authorities required")

    try:
        if not readback.at <= monotonic():
            return finish(Verdict.STOPPED, "REST readback time is in the future")

        qualify_by = readback.at + QUALIFICATION_SECONDS
        stop_at = readback.at + DELETE_CONTAINMENT_SECONDS if deleting else qualify_by
        streak = dict.fromkeys(servers, 0)
        while (started := monotonic()) < stop_at:
            round_end = min(started + POLL_SECONDS, stop_at)
            replies = read_pair(servers, name, round_end - started)
            received = monotonic()
            for reply in replies:
                record(reply, reply.at - readback.at)

            if received >= stop_at:
                break
            if received > round_end:
                return finish(Verdict.STOPPED, "DNS query round exceeded its timeout")
            if len(replies) != 2 or {reply.server for reply in replies} != set(servers):
                return finish(Verdict.STOPPED, "missing or unexpected authority")
            for reply in replies:
                if reply.name != name or not (started <= reply.at <= received):
                    return finish(Verdict.STOPPED, "response does not match the current query")
                if not reply.authoritative or reply.rcode in {"SERVFAIL", "REFUSED"}:
                    return finish(Verdict.STOPPED, "authority failed or refused")

                absent = reply.rcode == "NXDOMAIN" and reply.answer_count == reply.exact_a_count == 0
                present = reply.rcode == "NOERROR" and 0 < reply.exact_a_count <= reply.answer_count
                if not (present or absent):
                    return finish(Verdict.STOPPED, "unexpected answer for the disposable record")
                matches = absent if deleting else present
                if matches:
                    streak[reply.server] += 1
                elif streak[reply.server] >= 2:
                    return finish(Verdict.STOPPED, "authority regressed after two matching reads")
                else:
                    streak[reply.server] = 0

            if all(count >= 2 for count in streak.values()):
                if max(reply.at for reply in replies) < qualify_by:
                    return finish(Verdict.PASSED, "both authorities converged")
                return finish(Verdict.CONTAINED, "both authorities converged after qualification expired")

            delay = round_end - monotonic()
            if delay > 0:
                sleep(delay)

        return finish(Verdict.EXPIRED, "containment deadline missed" if deleting else "qualification deadline missed")
    except Exception:
        # Adapter and receipt failures cannot qualify or release an unresolved writer.
        return finish(Verdict.STOPPED, "could not complete DNS checks or receipt")
