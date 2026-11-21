"""Read staging API liveness and build SHA, not Cloudflare deployment identity."""

import json
import os
import re
import secrets
import subprocess
import time
from typing import NamedTuple

HEALTH_URL = "https://staging.nntune.com/healthz"
MAX_BODY_BYTES = 1024
MAX_SAMPLE_AGE_MS = 3_000


class UnsafeHealth(Exception):
    """Fixed classification only; does not expose response data."""


class BuildHealth(NamedTuple):
    observed_ms: int
    build_sha: str


def _unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def _validate(response, expected_sha):
    # Curl's header JSON can contain formatting newlines but not a raw record
    # separator. A raw separator is not valid in the health JSON body either.
    fields = response.rsplit(b"\x1e", 2)
    if len(fields) != 3:
        raise UnsafeHealth("shape")
    body, status, header_json = fields
    if status != b"200":
        raise UnsafeHealth("status")
    if len(body) > MAX_BODY_BYTES:
        raise UnsafeHealth("shape")
    try:
        headers = json.loads(
            header_json.decode("utf-8"), object_pairs_hook=_unique_fields
        )
    except (ValueError, RecursionError):
        raise UnsafeHealth("cache-unverified") from None
    if type(headers) is not dict or any(key != key.lower() for key in headers):
        raise UnsafeHealth("cache-unverified")
    cache_control = headers.get("cache-control")
    if not (
        isinstance(cache_control, list)
        and len(cache_control) == 1
        and isinstance(cache_control[0], str)
        and cache_control[0].lower() == "no-store"
        and headers.get("cf-cache-status") in (["DYNAMIC"], ["BYPASS"])
        and "age" not in headers
    ):
        raise UnsafeHealth("cache-unverified")
    try:
        payload = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_fields)
    except (ValueError, RecursionError):
        raise UnsafeHealth("shape") from None
    if type(payload) is not dict or set(payload) != {"ok", "build_sha"}:
        raise UnsafeHealth("shape")
    if payload["ok"] is not True or payload["build_sha"] != expected_sha:
        raise UnsafeHealth("unhealthy-or-wrong-build")


def _run_curl(run, args, timeout_ms):
    try:
        return run(
            ["curl", "--disable", *args],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=timeout_ms / 1000,
            env={"PATH": os.defpath, "LC_ALL": "C"},
        )
    except subprocess.TimeoutExpired:
        raise UnsafeHealth("deadline") from None
    except (OSError, ValueError):
        raise UnsafeHealth("transport") from None


def _require_streaming_cap(result):
    # Before 8.4, --max-filesize only worked when the size was known upfront.
    first_line = result.stdout.partition(b"\n")[0]
    for prefix in (rb"^curl ", rb" libcurl/"):
        version = re.search(
            prefix + rb"(\d{1,3})\.(\d{1,3})\.\d{1,3}(?:\s|$)", first_line
        )
        if (
            result.returncode != 0
            or version is None
            or tuple(map(int, version.groups())) < (8, 4)
        ):
            raise UnsafeHealth("curl-version")


def sample_build_health(
    expected_sha, *, deadline_ms, run=subprocess.run, monotonic=time.monotonic
):
    """Take one uncached sample with curl >=8.4, timestamped at probe start."""
    if not isinstance(expected_sha, str) or not re.fullmatch(
        r"[0-9a-f]{40}", expected_sha
    ):
        raise UnsafeHealth("expected-build")
    if type(deadline_ms) is not int or deadline_ms < 0:
        raise UnsafeHealth("deadline")
    args = [
        "--silent",
        "--proto",
        "=https",
        # A fresh HTTP/1.1 transfer avoids HTTP/2 refused-stream retries.
        "--http1.1",
        "--noproxy",
        "*",
        "--max-filesize",
        str(MAX_BODY_BYTES),
        "--header",
        "Accept-Encoding: identity",
        "--header",
        "Cache-Control: no-cache, no-store",
        "--write-out",
        "\x1e%{http_code}\x1e%{header_json}",
        f"{HEALTH_URL}?probe={secrets.token_hex(16)}",
    ]
    preflight_ms = monotonic() * 1000
    cutoff_ms = min(deadline_ms, int(preflight_ms) + MAX_SAMPLE_AGE_MS)
    if not preflight_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    _require_streaming_cap(_run_curl(run, ("--version",), cutoff_ms - preflight_ms))
    started_ms = monotonic() * 1000
    if not preflight_ms <= started_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    result = _run_curl(run, args, cutoff_ms - started_ms)
    returned_ms = monotonic() * 1000
    if not started_ms <= returned_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    if result.returncode != 0:
        raise UnsafeHealth("transport")
    _validate(result.stdout, expected_sha)
    if not returned_ms <= monotonic() * 1000 < cutoff_ms:
        raise UnsafeHealth("deadline")
    return BuildHealth(int(started_ms), expected_sha)
