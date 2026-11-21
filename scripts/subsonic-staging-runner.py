#!/usr/bin/env python3
"""Rehearse the reviewed Subsonic schedule against an in-memory staging model."""

import argparse
import hashlib
import json
import math
import os
import re
import signal
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path
from selectors import EVENT_READ, EVENT_WRITE, DefaultSelector
from urllib.parse import quote_plus, urlencode

MATRIX = Path(__file__).resolve().parents[1] / "fixtures/subsonic/staging/matrix.json"
MATRIX_DIGEST = "18a076d2fc86f2a7dadf5efd2e1a1200c741b821379afeb158c4dac06ce80abb"
MATRIX_LIMIT = 32 * 1024
STAGING_ORIGIN = "https://staging.nntune.com"
FORM_LIMIT = 8 * 1024
ENCODED_API_KEY_LIMIT = 1024  # MAX_CREDENTIAL_VALUE_BYTES in subsonic_http.rs
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
    _validate_credentials(bearer, api_key)
    return (_prepare_post(slot, bearer, api_key) for slot in matrix["slots"])


def _validate_credentials(bearer, api_key):
    require(
        type(bearer) is str and re.fullmatch(r"[!-~]{1,1024}", bearer),
        "credential-shape",
    )
    require(
        type(api_key) is str
        and re.fullmatch(r"[!-~]{32,512}", api_key)
        and len(quote_plus(api_key)) <= ENCODED_API_KEY_LIMIT,
        "credential-shape",
    )


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
    sample = {"oracle": oracle, "http_status": status}
    if "body_bytes" in expected:
        sample["body_bytes"] = len(body)
    return sample


@dataclass(frozen=True)
class PostResponse:
    status: int
    body: bytes = field(repr=False)
    content_type: str | None = field(repr=False)
    cache_control: str | None = field(repr=False)
    retry_after: str | None = field(repr=False)


def _curl_config(post):
    def quoted(value):
        return '"' + value.replace('\\', '\\\\').replace('"', '\\"') + '"'

    lines = [f"header = {quoted(f'{name}: {value}')}" for name, value in post.headers]
    lines.append(f"data-binary = {quoted(post.body.decode('ascii'))}")
    return ("\n".join(lines) + "\n").encode("ascii")


_RESPONSE_FIELDS = (b"content-type", b"cache-control", b"retry-after")
_RESPONSE_HEADER_LIMIT = 8192


def _response_head(raw, cap):
    status_line, *lines = bytes(raw).split(b"\r\n")
    match = re.fullmatch(rb"HTTP/1\.[01] ([2-5][0-9]{2})(?: [ -~]*)?", status_line)
    require(match, "response-headers")
    status = int(match[1])
    require(not 300 <= status < 400, "redirect-refused")
    fields = {}
    for line in lines:
        name, sep, value = line.partition(b":")
        require(sep and re.fullmatch(rb"[!#$%&'*+.^_`|~0-9A-Za-z-]+", name), "response-headers")
        name = name.lower()
        if name in (*_RESPONSE_FIELDS, b"content-length", b"content-encoding", b"transfer-encoding"):
            require(name not in fields and re.fullmatch(rb"[\t -~]*", value), "response-headers")
            fields[name] = value.strip().decode("ascii")
    require(fields.get(b"content-encoding", "identity").lower() == "identity", "response-headers")
    length = fields.get(b"content-length")
    if b"transfer-encoding" in fields:
        require(length is None and fields[b"transfer-encoding"].lower() == "chunked", "response-headers")
    if length is not None:
        require(length.isdecimal(), "response-headers")
        length = length.lstrip("0") or "0"
        require(len(length) <= len(str(cap)) and int(length) <= cap, "body-limit")
        length = int(length)
    return status, length, tuple(fields.get(name) for name in _RESPONSE_FIELDS)


class StagingPostTransport:
    """One reviewed slot per call. The executor still owns scheduling and server evidence."""

    def __init__(self, *, spawn=subprocess.Popen, clock=time.monotonic):
        self._matrix = load_matrix(MATRIX)
        self._spawn = spawn  # Trusted dependency injection for disposable clients only.
        self._clock = clock

    def _remaining(self, deadline, cancel):
        require(not cancel.is_set(), "cancelled")
        remaining = deadline - self._clock()
        require(remaining > 0, "request-or-reader-timeout")
        return min(0.05, remaining)

    def send(self, number, bearer, api_key, *, deadline, cancel):
        """Return a PostResponse or a sanitized fixed-rejection/client-ended sample.

        deadline is the executor's absolute monotonic cutoff; cancel is a threading.Event.
        Credentials are the staging measurement credentials, not health Access tokens.
        """
        slots, limits = self._matrix["slots"], self._matrix["limits"]
        require(type(number) is int and 1 <= number <= len(slots), "unreviewed-slot")
        require(type(deadline) is float and math.isfinite(deadline), "invalid-deadline")
        slot = slots[number - 1]
        _validate_credentials(bearer, api_key)
        post = _prepare_post(slot, bearer, api_key)
        require(post.method == "POST" and post.url == f"{STAGING_ORIGIN}/rest/{slot['endpoint']}",
                "destination-refused")
        reader = slot.get("reader", "normal")
        timeout_ms = limits["request_timeout_ms"]
        if reader != "normal":
            timeout_ms = min(timeout_ms, limits["reader_hold_max_ms"])
        start = self._clock()
        end = min(deadline, start + timeout_ms / 1000)
        # Withhold even pipe reads for paired legs; curl, TLS and kernel buffering still exist.
        read_after = start + {"normal": 0, "held": 1.5, "disconnect": 1.1}[reader]
        oracle = slot["expected"]["oracle"]
        cap = REJECTION_LIMIT if oracle in REJECTION_ORACLES else limits["body_bytes"]
        argv = (
            "/usr/bin/curl", "--disable", "--globoff", "--proto", "=https", "--noproxy", "*",
            "--no-location", "--retry", "0", "--http1.1", "--silent", "--no-buffer", "--include",
            "--connect-timeout", str(limits["connect_timeout_ms"] / 1000),
            "--max-time", str(timeout_ms / 1000), "--request", "POST", "--config", "-",
            "--url", post.url,
        )
        self._remaining(end, cancel)
        try:
            process = self._spawn(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.DEVNULL, env={}, close_fds=True)
            try:
                response = self._exchange(process, _curl_config(post), cap, end, cancel,
                                          read_after, reader == "disconnect")
            finally:
                try:
                    if process.poll() is None:
                        process.kill()
                    process.wait(timeout=limits["cleanup_timeout_ms"] / 1000)
                finally:
                    process.stdin.close()
                    process.stdout.close()
        except subprocess.TimeoutExpired:
            raise Stop("client-reap-timeout") from None
        except OSError:
            raise Stop("transport-io") from None
        self._remaining(end, cancel)
        if response is None:
            require(process.returncode == -signal.SIGKILL, "disconnect-unconfirmed")
            return {"oracle": "client-ended"}  # Local client only; never evidence of server drain.
        if oracle in REJECTION_ORACLES:
            return classify_fixed_rejection(slot, response.status, response.body,
                                            response.content_type, response.cache_control,
                                            response.retry_after)
        return response

    def _exchange(self, process, config, cap, end, cancel, read_after, disconnect):
        head, body = bytearray(), bytearray()
        response_head, eof, reading = None, False, False
        with DefaultSelector() as selector:
            os.set_blocking(process.stdin.fileno(), False)
            os.set_blocking(process.stdout.fileno(), False)
            selector.register(process.stdin, EVENT_WRITE)
            while True:
                wait = self._remaining(end, cancel)
                until_read = read_after - self._clock()
                if not reading and until_read <= 0:
                    selector.register(process.stdout, EVENT_READ)
                    reading = True
                if eof and process.poll() is not None:
                    break
                if not reading and until_read > 0:
                    wait = min(wait, until_read)
                if not selector.get_map():
                    cancel.wait(wait)
                    continue
                for key, _ in selector.select(wait):
                    self._remaining(end, cancel)
                    if key.fileobj is process.stdin:
                        config = config[os.write(key.fd, config):]
                        if not config:
                            selector.unregister(key.fileobj)
                            process.stdin.close()
                        continue
                    if response_head:
                        read_size = min(8192, cap - len(body) + 1)
                    else:
                        read_size = min(REJECTION_LIMIT, _RESPONSE_HEADER_LIMIT - len(head) + 1)
                    chunk = os.read(key.fd, read_size)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        eof = True
                        continue
                    if not response_head:
                        head.extend(chunk)
                        offset = head.find(b"\r\n\r\n")
                        if offset < 0:
                            require(len(head) < _RESPONSE_HEADER_LIMIT, "response-headers")
                            continue
                        require(offset + 4 <= _RESPONSE_HEADER_LIMIT, "response-headers")
                        response_head = _response_head(head[:offset], cap)
                        if disconnect:
                            status, length, _ = response_head
                            require(status == 200 and length != 0, "response-mismatch")
                            require(process.poll() is None, "disconnect-unconfirmed")
                            return None
                        chunk = head[offset + 4:]
                        head.clear()
                    require(len(body) + len(chunk) <= cap, "body-limit")
                    body.extend(chunk)
        require(process.returncode != 28, "request-or-reader-timeout")  # CURLE_OPERATION_TIMEDOUT
        require(process.returncode == 0 and response_head is not None, "transport-incomplete")
        status, length, fields = response_head
        require(length is None or len(body) == length, "transport-incomplete")
        return PostResponse(status, bytes(body), *fields)


@dataclass(frozen=True)
class Deployment:
    deployment_id: str
    version: str
    serving_version: str
    enabled: bool
    lease: bool
    measurement_binding: bool

    def verified_off(self):
        return (
            bool(self.deployment_id and self.version)
            and self.serving_version == self.version
            and self.enabled is False
            and self.lease is False
            and self.measurement_binding is False
        )


BASELINE = Deployment("deploy-off", "baseline", "baseline", False, False, False)
EXPERIMENT = Deployment("deploy-on", "experiment", "experiment", True, True, True)


class StagingModel:
    """Synthetic clock, telemetry and readback; no transport or provider calls."""

    def __init__(self):
        self.now = 0
        self.state = BASELINE
        self.pending = {}

    def tick(self):
        self.now += TICK_MS

    def telemetry(self):
        return self.now, True, len(self.pending)

    def arm(self):
        require(self.state == BASELINE, "preflight-not-off-and-idle")
        self.state = EXPERIMENT

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

    def restore_if_current(self, expected_deployment_id):
        # Atomic only in this model. A provider read then unconditional write is not equivalent.
        if self.state.deployment_id != expected_deployment_id:
            return None
        self.state = Deployment("deploy-restored", BASELINE.version, BASELINE.version,
                                False, False, False)
        return self.state.deployment_id

    def readback(self):
        return self.state


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


def cleanup(model, limits):
    deadline = model.now + limits["cleanup_timeout_ms"]
    restored_id = None
    try:
        model.cancel_clients()
    finally:
        current = model.readback()
        if (current.deployment_id, current.version) == (EXPERIMENT.deployment_id, EXPERIMENT.version):
            restored_id = model.restore_if_current(EXPERIMENT.deployment_id)

    off = model.readback()
    require(off.verified_off() and off.version != EXPERIMENT.version
            and off.deployment_id != EXPERIMENT.deployment_id, "restore-unverified")
    owned = off.deployment_id in (BASELINE.deployment_id, restored_id)
    require(not owned or off.version == BASELINE.version, "restore-unverified")
    # Advance before sampling: a zero observed at/before the off readback is not drain evidence.
    while model.now < deadline:
        advance(model)
        require(model.now <= deadline, "restore-unverified")
        if pressure(model, limits) == 0:
            require(model.readback() == off, "restore-unverified")
            return "simulated-off-and-drained" if owned else "simulated-superseded-off-and-drained"
    return "unverified"


def rehearse(path, model):
    report = {"mode": "offline-simulation", "stop": "complete", "cleanup": "not-armed",
              "starts": 0, "max_simulated_clients": 0, "max_simulated_backends": None,
              "simulated_pair_overlap": {}, "slots": {}}
    cleanup_needed = False
    try:
        matrix = load_matrix(path)
        report["slots"] = {slot["slot"]: {"state": "unstarted"} for slot in matrix["slots"]}
        limits = matrix["limits"]
        baseline = model.readback()
        require(baseline == BASELINE and baseline.verified_off() and pressure(model, limits) == 0,
                "preflight-not-off-and-idle")
        cleanup_needed = True  # Even a failed arm can have changed state.
        start = model.now
        model.arm()
        require(model.readback() == EXPERIMENT, "serving-version-unverified")
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
        if cleanup_needed:
            report["cleanup"] = "unverified"
            try:
                report["cleanup"] = cleanup(model, limits)
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
