#!/usr/bin/env python3
"""Rehearse the reviewed Subsonic schedule against an in-memory staging model."""

import argparse
import hashlib
import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from urllib.parse import urlencode

MATRIX = Path(__file__).resolve().parents[1] / "fixtures/subsonic/staging/matrix.json"
MATRIX_DIGEST = "18a076d2fc86f2a7dadf5efd2e1a1200c741b821379afeb158c4dac06ce80abb"
MATRIX_LIMIT = 32 * 1024
STAGING_ORIGIN = "https://staging.nntune.com"
FORM_LIMIT = 8 * 1024
REJECTION_LIMIT = 1024
TICK_MS = 50

REJECTION_ORACLES = {
    "fixed_staging_off": (404, "xml", 70, "Endpoint not supported in staging.", None),
    "fixed_capacity_json": (503, "json", 0, "Service unavailable.", "1"),
    "fixed_oversize_no_callback": (
        413,
        None,
        0,
        "Request or document too large.",
        None,
    ),
}


class Stop(Exception):
    """A fixed classification safe to include in an offline report."""


def require(ok, reason):
    if not ok:
        raise Stop(reason)


def unique_object(pairs):
    result = dict(pairs)
    require(len(result) == len(pairs), "unreviewed-matrix")
    return result


def load_matrix(path):
    try:
        with path.open("rb") as source:
            raw = source.read(MATRIX_LIMIT + 1)
        require(len(raw) <= MATRIX_LIMIT, "unreviewed-matrix")
        matrix = json.loads(raw, object_pairs_hook=unique_object)
        encoded = json.dumps(matrix, sort_keys=True, separators=(",", ":")).encode()
    except (OSError, ValueError, RecursionError):
        raise Stop("unreviewed-matrix") from None
    require(hashlib.sha256(encoded).hexdigest() == MATRIX_DIGEST, "unreviewed-matrix")
    return matrix


@dataclass(frozen=True)
class PreparedPost:
    slot: int
    url: str
    headers: tuple[tuple[str, str], ...] = field(repr=False)
    body: bytes = field(repr=False)
    method: str = field(default="POST", init=False)


def prepare_requests(path, bearer, api_key):
    """Build unsent POSTs from only the reviewed schedule, with no live CLI."""
    matrix = load_matrix(path)
    require(
        isinstance(bearer, str) and re.fullmatch(r"[!-~]{1,1024}", bearer),
        "credential-shape",
    )
    require(
        isinstance(api_key, str) and re.fullmatch(r"[!-~]{32,512}", api_key),
        "credential-shape",
    )
    return (_prepare_post(slot, bearer, api_key) for slot in matrix["slots"])


def _prepare_post(slot, bearer, api_key):
    auth = slot.get("auth")
    params = dict(slot["params"])
    if auth is None:
        params["apiKey"] = api_key
    body = urlencode(params).encode("ascii")
    require(len(body) <= FORM_LIMIT, "form-limit")
    headers = (
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Accept-Encoding", "identity"),
        ("X-Subsonic-Measurement-Case", slot["case"]),
    )
    if auth != "omit_both":
        headers += (("Authorization", f"Bearer {bearer}"),)
    return PreparedPost(
        slot["slot"], f"{STAGING_ORIGIN}/rest/{slot['endpoint']}", headers, body
    )


def classify_fixed_rejection(
    slot, status, body, content_type, cache_control, retry_after
):
    """Accept only the application's pinned rejection, not a proxy status or pair outcome."""
    expected = slot["expected"]
    oracle = expected["oracle"]
    require(oracle in REJECTION_ORACLES, "response-mismatch")
    wanted_status, fmt, code, message, wanted_retry = REJECTION_ORACLES[oracle]
    if fmt is None:
        # An over-cap JSONP response must be plain JSON, with no callback.
        fmt = "json" if slot["params"].get("f") in ("json", "jsonp") else "xml"
    if fmt == "json":
        payload = {
            "subsonic-response": {
                "status": "failed",
                "version": "1.16.1",
                "error": {"code": code, "message": message},
            }
        }
        wanted_body = json.dumps(payload, separators=(",", ":")).encode("ascii")
    else:
        wanted_body = (
            f'<subsonic-response status="failed" version="1.16.1">'
            f'<error code="{code}" message="{message}"/></subsonic-response>'
        ).encode("ascii")
    require(len(body) <= REJECTION_LIMIT, "body-limit")
    require(
        status == wanted_status == expected["http_status"]
        and content_type == f"application/{fmt}"
        and cache_control == "private, no-store"
        and retry_after == wanted_retry
        and body == wanted_body
        and len(body) == expected.get("body_bytes", len(wanted_body)),
        "response-mismatch",
    )
    return {"oracle": oracle, "http_status": status, "body_bytes": len(body)}


class StagingModel:
    """Synthetic clock, telemetry and readback; no transport or provider calls."""

    def __init__(self):
        self.now = 0
        self.version = "baseline"
        self.lease = False
        self.pending = {}

    def tick(self):
        self.now += TICK_MS

    def telemetry(self):
        return self.now, True, len(self.pending)

    def arm(self):
        self.version, self.lease = "experiment", True

    def start(self, slot):
        delay = {"normal": 100, "held": 1500, "disconnect": 1100}[slot.get("reader", "normal")]
        self.pending[slot["slot"]] = self.now + delay

    def result(self, slot):
        number = slot["slot"]
        if self.now < self.pending[number]:
            return None
        del self.pending[number]
        if slot.get("reader") == "disconnect":
            return {"oracle": "client-ended"}
        if "pair" in slot:
            return {"oracle": "large_complete_album", "http_status": 200, "body_bytes": 106659}
        return slot["expected"].copy()

    def busy_observed(self, slot):
        return False

    def cancel_clients(self):
        self.pending.clear()

    def restore(self, baseline):
        # Do not overwrite a newer deployment; verify its off-state instead.
        if self.version == "experiment":
            self.version, self.lease = baseline, False

    def readback(self):
        return self.version, self.lease


def advance(model):
    previous = model.now
    model.tick()
    require(model.now > previous, "clock-or-window")


def pressure(model, limits):
    at, healthy, backends = model.telemetry()
    require(at == model.now and healthy is True, "telemetry-stale-or-unsafe")
    require(type(backends) is int and 0 <= backends <= limits["experiment_db_connections"],
            "backend-pressure")
    return backends


def validate(model, slot, sample, elapsed, limits):
    require(0 <= sample.get("body_bytes", 0) <= limits["body_bytes"], "body-limit")
    if "pair" not in slot:
        require(sample == slot["expected"], "response-mismatch")
    elif slot.get("reader") == "disconnect":
        require(sample == {"oracle": "client-ended"}, "disconnect-unconfirmed")
    elif sample in ({"oracle": "fixed_capacity_json", "http_status": 503, "body_bytes": 112},
                    {"oracle": "fixed_capacity_xml", "http_status": 503, "body_bytes": 120}):
        require(model.busy_observed(slot) is True, "unattributed-pair-result")
        require(elapsed <= limits["busy_rejection_max_ms"], "busy-latency")
    else:
        require(sample == {"oracle": "large_complete_album", "http_status": 200, "body_bytes": 106659},
                "unattributed-pair-result")
    if sample.get("http_status") == 200 and slot.get("reader", "normal") == "normal":
        require(elapsed <= limits["ordinary_success_max_ms"], "success-latency")


def execute(model, matrix, report, cutoff):
    limits = matrix["limits"]
    remaining = iter(matrix["slots"])
    next_slot = next(remaining, None)
    active = {}
    last_start = -limits["min_start_interval_ms"]
    while next_slot or active:
        require(model.now < cutoff, "clock-or-window")
        backends = pressure(model, limits)
        peak = report["max_simulated_backends"]
        report["max_simulated_backends"] = backends if peak is None else max(peak, backends)
        for number, (slot, started) in list(active.items()):
            elapsed = model.now - started
            deadline = limits["request_timeout_ms"]
            if slot.get("reader", "normal") != "normal":
                deadline = min(deadline, limits["reader_hold_max_ms"])
            require(elapsed <= deadline, "request-or-reader-timeout")
            sample = model.result(slot)
            if sample is None:
                require(elapsed < deadline, "request-or-reader-timeout")
                continue
            row = report["slots"][number]
            row.update(state="rejected", elapsed_ms=elapsed)
            validate(model, slot, sample, elapsed, limits)
            row.update(sample, state="accepted")
            del active[number]
        if next_slot and model.now - last_start >= limits["min_start_interval_ms"]:
            paired = next_slot.get("leg") == 2
            same_pair = all(slot.get("pair") == next_slot.get("pair") for slot, _ in active.values())
            # Wait for independent backend drain before a new group, not just for client EOF.
            if (not active and backends == 0) or (
                paired and active and same_pair and backends < limits["experiment_db_connections"]
            ):
                require(model.now + limits["request_timeout_ms"] < cutoff, "window-reserve")
                require(len(active) < limits["in_flight"] and report["starts"] < limits["request_starts"],
                        "start-limit")
                if paired:
                    report["simulated_pair_overlap"][next_slot["pair"]] = bool(active)
                report["slots"][next_slot["slot"]]["state"] = "unfinished"
                report["starts"] += 1
                last_start = model.now
                active[next_slot["slot"]] = (next_slot, last_start)
                report["max_simulated_clients"] = max(report["max_simulated_clients"], len(active))
                model.start(next_slot)
                next_slot = next(remaining, None)
        if next_slot or active:
            advance(model)


def rehearse(path, model):
    report = {"mode": "offline-simulation", "stop": "complete", "cleanup": "not-armed",
              "starts": 0, "max_simulated_clients": 0, "max_simulated_backends": None,
              "simulated_pair_overlap": {}, "slots": {}}
    baseline = None
    try:
        matrix = load_matrix(path)
        report["slots"] = {slot["slot"]: {"state": "unstarted"} for slot in matrix["slots"]}
        limits = matrix["limits"]
        version, lease = model.readback()
        require(version and lease is False and pressure(model, limits) == 0, "preflight-not-off-and-idle")
        baseline = version  # Arm can fail after changing state, so cleanup owns this first.
        start = model.now
        model.arm()
        cutoff = start + min(limits["load_window_ms"], limits["lease_max_ms"],
                             limits["job_timeout_ms"] - limits["cleanup_timeout_ms"])
        execute(model, matrix, report, cutoff)
    except Stop as error:
        report["stop"] = str(error)
    except KeyboardInterrupt:
        report["stop"] = "cancelled"
    except Exception:
        report["stop"] = "model-failure"
    finally:
        if baseline is not None:
            report["cleanup"] = "unverified"
            deadline = model.now + limits["cleanup_timeout_ms"]
            try:
                try:
                    model.cancel_clients()
                finally:
                    model.restore(baseline)
                while model.now <= deadline:
                    restored, live_lease = model.readback()
                    require(restored and restored != "experiment" and live_lease is False, "restore-unverified")
                    if pressure(model, limits) == 0:
                        report["cleanup"] = "simulated-off-and-drained"
                        break
                    advance(model)
            except (Exception, KeyboardInterrupt):
                pass  # Leave cleanup unverified; never echo adapter exceptions.
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--simulate", action="store_true", required=True)
    parser.parse_args()
    report = rehearse(MATRIX, StagingModel())
    print(json.dumps(report))
    return int(report["stop"] != "complete" or report["cleanup"] != "simulated-off-and-drained")


if __name__ == "__main__":
    raise SystemExit(main())
