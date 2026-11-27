"""Read-only, point-in-time verification of Subsonic staging off and drained."""

import time
from enum import StrEnum
from typing import NamedTuple

from staging_api_identity import SHA, UUID, current_deployment, verify_version
from subsonic_staging_observer import MAX_SAMPLE_AGE_SECONDS, PressureObservation


class DeploymentPin(NamedTuple):
    deployment_id: str
    version_id: str
    build_sha: str


class CleanupResult(StrEnum):
    VERIFIED = "verified"
    INVALID_PIN = "invalid-pin"
    DEADLINE = "deadline"
    DEPLOYMENT_UNVERIFIED = "deployment-unverified"
    OFF_UNVERIFIED = "off-unverified"
    DRAIN_UNVERIFIED = "drain-unverified"
    MEASUREMENT_SESSIONS_PRESENT = "measurement-sessions-present"


class _Deadline(Exception):
    pass


def verify_cleanup(
    pin: DeploymentPin,
    *,
    deadline_ms: int,
    read_deployments,
    read_version,
    sample_pressure,
    monotonic=time.monotonic,
) -> CleanupResult:
    """Require fresh staging-only reads; return a static result, not evidence bodies.

    read_deployments(*, deadline_ms) and read_version(version_id, *, deadline_ms)
    return decoded provider results, never saved receipts. The version is immutable.
    sample_pressure(*, deadline_ms) uses the independent one-shot observer with
    its environment bound by the caller. The observer and verifier share the
    same monotonic clock (seconds); deadlines and observations use milliseconds.
    Callbacks must be read-only, silent, uncached and enforce the absolute
    deadline themselves: this synchronous verifier cannot interrupt a callback.
    """
    if type(pin) is not DeploymentPin or not all(
        type(value) is str and pattern.fullmatch(value) for pattern, value in zip((UUID, UUID, SHA), pin)
    ):
        return CleanupResult.INVALID_PIN
    if type(deadline_ms) is not int or deadline_ms < 0:
        return CleanupResult.DEADLINE

    now_ms = 0

    def check_clock():
        nonlocal now_ms
        try:
            reading = monotonic()
        except Exception:  # noqa: BLE001 (caller-supplied clock)
            raise _Deadline from None
        if type(reading) in (int, float):
            ms = reading * 1000
            if now_ms <= ms < deadline_ms:
                now_ms = ms
                return ms
        raise _Deadline

    def read(callback, *args):
        started_ms = check_clock()
        try:
            return started_ms, callback(*args, deadline_ms=deadline_ms)
        finally:
            check_clock()

    def matches_pin():
        _, deployments = read(read_deployments)
        return current_deployment(deployments, pin.version_id) == pin.deployment_id

    def fresh(row):
        return now_ms - row.observed_ms < MAX_SAMPLE_AGE_SECONDS * 1000

    failure = CleanupResult.DEPLOYMENT_UNVERIFIED
    try:
        if not matches_pin():
            return failure
        failure = CleanupResult.OFF_UNVERIFIED
        _, version = read(read_version, pin.version_id)
        verify_version(version, pin.version_id, pin.build_sha)
        failure = CleanupResult.DEPLOYMENT_UNVERIFIED
        if not matches_pin():
            return failure
        off_fence_ms = check_clock()

        failure = CleanupResult.DRAIN_UNVERIFIED
        dispatched_ms, row = read(sample_pressure)
        if (
            type(row) is not PressureObservation
            or type(row.observed_ms) not in (int, float)
            or not off_fence_ms < row.observed_ms <= now_ms
            or row.observed_ms < dispatched_ms
            or not fresh(row)
            or type(row.measurement) is not int
            or row.measurement < 0
            or type(row.ordinary_free) is not int
        ):
            return failure
        if row.measurement != 0:
            return CleanupResult.MEASUREMENT_SESSIONS_PRESENT

        failure = CleanupResult.DEPLOYMENT_UNVERIFIED
        if not matches_pin():
            return failure
        check_clock()
        return CleanupResult.VERIFIED if fresh(row) else CleanupResult.DRAIN_UNVERIFIED
    except _Deadline:
        return CleanupResult.DEADLINE
    except Exception:  # noqa: BLE001 (redact callback exceptions and parser diagnostics)
        return failure
