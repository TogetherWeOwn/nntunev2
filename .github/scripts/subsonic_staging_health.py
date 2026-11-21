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
    fields = response.rsplit(b"\n", 4)
    if len(fields) != 5:
        raise UnsafeHealth("shape")
    body, status, cache_control, cache_status, age = fields
    if status != b"200":
        raise UnsafeHealth("status")
    if (
        cache_control.lower() != b"no-store"
        or cache_status not in (b"DYNAMIC", b"BYPASS")
        or age
    ):
        raise UnsafeHealth("cache-unverified")
    if len(body) > MAX_BODY_BYTES:
        raise UnsafeHealth("shape")
    try:
        payload = json.loads(body.decode("utf-8"), object_pairs_hook=_unique_fields)
    except (ValueError, RecursionError):
        raise UnsafeHealth("shape") from None
    if type(payload) is not dict or set(payload) != {"ok", "build_sha"}:
        raise UnsafeHealth("shape")
    if payload["ok"] is not True or payload["build_sha"] != expected_sha:
        raise UnsafeHealth("unhealthy-or-wrong-build")


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
    argv = [
        "curl",
        "--disable",
        "--silent",
        "--proto",
        "=https",
        "--noproxy",
        "*",
        "--max-filesize",
        str(MAX_BODY_BYTES),
        "--header",
        "Accept-Encoding: identity",
        "--header",
        "Cache-Control: no-cache, no-store",
        "--write-out",
        "\n%{http_code}\n%header{cache-control}\n%header{cf-cache-status}\n%header{age}",
        f"{HEALTH_URL}?probe={secrets.token_hex(16)}",
    ]
    started_ms = monotonic() * 1000
    observed_ms = int(started_ms)
    cutoff_ms = min(deadline_ms, observed_ms + MAX_SAMPLE_AGE_MS)
    if not started_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    try:
        result = run(
            argv,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=(cutoff_ms - started_ms) / 1000,
            env={"PATH": os.defpath, "LC_ALL": "C"},
        )
    except subprocess.TimeoutExpired:
        raise UnsafeHealth("deadline") from None
    except (OSError, ValueError):
        raise UnsafeHealth("transport") from None
    returned_ms = monotonic() * 1000
    if not started_ms <= returned_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    if result.returncode != 0:
        raise UnsafeHealth("transport")
    _validate(result.stdout, expected_sha)
    if not returned_ms <= monotonic() * 1000 < cutoff_ms:
        raise UnsafeHealth("deadline")
    return BuildHealth(observed_ms, expected_sha)
