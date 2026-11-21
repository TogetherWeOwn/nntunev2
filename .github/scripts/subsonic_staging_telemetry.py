"""Offline admission policy for validated, trusted staging telemetry; no collectors."""

from dataclasses import dataclass

MAX_SAMPLE_AGE_MS = 3_000
MAX_LOAD_MS = 120_000
MAX_RECONCILE_MS = 120_000
MAX_BACKENDS = 2


class TelemetryStop(Exception):
    """Fixed category only; never includes raw telemetry."""


@dataclass(frozen=True)
class Pressure:
    observed_ms: int
    measurement: int
    ordinary_free: int


@dataclass(frozen=True)
class Health:
    observed_ms: int
    serving_version: str
    healthy: bool


@dataclass(frozen=True)
class ResourceCoverage:
    serving_version: str
    covered_from_ms: int
    covered_through_ms: int
    exceeded_or_terminated: int


def nat(value):
    return type(value) is int and value >= 0


class TelemetryGate:
    """Use a common monotonic timeline for all normalized observations.

    Adapters must validate source timestamps/identity before normalization.
    Coverage means complete provider evidence, not the latest event timestamp
    or an empty response. Only platform resource events may use delayed mode.
    """

    def __init__(self, opened_ms, serving_version, *, delayed_resources=False):
        if (
            not nat(opened_ms)
            or not isinstance(serving_version, str)
            or not serving_version
            or type(delayed_resources) is not bool
        ):
            raise TelemetryStop("telemetry-config")
        self._opened = self._last = opened_ms
        self._version = serving_version
        self._delayed = delayed_resources
        self._group_completed = None
        self._closed = None
        self._stop = None

    def _require(self, ok, category):
        if not ok:
            self._stop = self._stop or category
        if self._stop:
            raise TelemetryStop(self._stop)

    def _clock(self, now):
        self._require(nat(now) and now >= self._last, "telemetry-clock")
        self._last = now

    def _fresh(self, stamp, now):
        return nat(stamp) and max(self._opened, now - MAX_SAMPLE_AGE_MS) <= stamp <= now

    def _resources(self, now, coverage, required_through):
        if coverage is not None:
            self._require(
                isinstance(coverage, ResourceCoverage)
                and coverage.serving_version == self._version
                and nat(coverage.covered_from_ms)
                and nat(coverage.covered_through_ms)
                and coverage.covered_from_ms <= self._opened
                and coverage.covered_from_ms <= coverage.covered_through_ms <= now
                and nat(coverage.exceeded_or_terminated),
                "resource-coverage-invalid",
            )
            self._require(coverage.exceeded_or_terminated == 0, "resource-event")
        if required_through is not None:
            self._require(
                coverage is not None
                and coverage.covered_through_ms >= required_through,
                "resource-coverage-missing",
            )

    def _check_live(self, now, pressure, health, coverage):
        self._require(
            isinstance(pressure, Pressure) and self._fresh(pressure.observed_ms, now),
            "db-telemetry-missing-or-stale",
        )
        self._require(
            nat(pressure.measurement)
            and pressure.measurement <= MAX_BACKENDS
            and nat(pressure.ordinary_free)
            and pressure.ordinary_free >= MAX_BACKENDS - pressure.measurement,
            "db-pressure",
        )
        self._require(
            isinstance(health, Health) and self._fresh(health.observed_ms, now),
            "health-telemetry-missing-or-stale",
        )
        self._require(
            health.serving_version == self._version and health.healthy is True,
            "health-unsafe",
        )
        self._resources(
            now,
            coverage,
            None if self._delayed else max(self._opened, now - MAX_SAMPLE_AGE_MS),
        )

    def record_group_completion(self, now):
        """Call once all clients in a group terminate, before admitting a new group."""
        self._clock(now)
        self._require(
            self._closed is None and now <= self._opened + MAX_LOAD_MS, "load-window"
        )
        self._group_completed = now

    def observe(self, now, pressure, health, coverage, *, overlapping_pair=False):
        """Check live telemetry. False means wait; overlap is only for a pair's second client."""
        self._clock(now)
        self._require(
            self._closed is None and now < self._opened + MAX_LOAD_MS, "load-window"
        )
        self._require(type(overlapping_pair) is bool, "telemetry-config")
        self._check_live(now, pressure, health, coverage)
        if overlapping_pair:
            return pressure.measurement < MAX_BACKENDS
        return pressure.measurement == 0 and (
            self._group_completed is None
            or pressure.observed_ms > self._group_completed
        )

    def close_load(self, now, pressure, health, coverage):
        """Fence new starts after the last client terminates; not a cleanup check."""
        self._clock(now)
        self._require(
            self._closed is None and now <= self._opened + MAX_LOAD_MS, "load-window"
        )
        self._check_live(now, pressure, health, coverage)
        self._closed = now

    def reconcile(self, now, coverage):
        """Return False while awaiting complete resource coverage within the deadline."""
        self._clock(now)
        self._require(self._closed is not None, "load-not-closed")
        bound = MAX_RECONCILE_MS if self._delayed else MAX_SAMPLE_AGE_MS
        deadline = self._closed + bound
        self._require(now <= deadline, "resource-reconcile-deadline")
        self._resources(now, coverage, None)
        complete = coverage is not None and coverage.covered_through_ms >= self._closed
        self._require(complete or now < deadline, "resource-coverage-missing")
        return complete
