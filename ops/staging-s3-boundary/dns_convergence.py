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
    """The executor's two independent REST checks and their completion time."""

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
    name: str,
    servers: tuple[str, str],
    read_pair: Callable[[tuple[str, str], str, float], Sequence[Observation]],
    monotonic: Callable[[], float],
    sleep: Callable[[float], None],
    record: Callable[[Observation, float], None],
) -> Result:
    """Evaluate one exact name using two time-bounded direct queries per round.

    The adapter validates each packet against the query and pinned server,
    counts IN A answers for the exact name, and represents a missing or invalid
    packet as a failed observation for that server. It starts both queries
    within the same round and returns within the supplied timeout in seconds.
    The caller owns the writer and persists the record callback in private.
    """
    deleting = change is Change.DELETE

    def finish(verdict: Verdict, reason: str) -> Result:
        return Result(verdict, reason, deleting and verdict is not Verdict.PASSED)

    if not (readback.exact_id and readback.exact_name and readback.at <= monotonic()):
        return finish(Verdict.STOPPED, "missing exact REST readback")
    identities = {server.rstrip(".").lower() for server in servers}
    if not name or len(servers) != 2 or len(identities) != 2 or "" in identities:
        return finish(Verdict.STOPPED, "exact name and two distinct authorities required")

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
            if received < qualify_by:
                return finish(Verdict.PASSED, "both authorities converged")
            return finish(Verdict.CONTAINED, "both authorities converged after qualification expired")

        delay = round_end - monotonic()
        if delay > 0:
            sleep(delay)

    return finish(Verdict.EXPIRED, "containment deadline missed" if deleting else "qualification deadline missed")
