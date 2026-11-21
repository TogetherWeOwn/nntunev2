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
    # Direct Worker responses have no CF cache status; a present one must agree.
    if (
        headers.get("cache-control") != ["no-store"]
        or "age" in headers
        or headers.get("cf-cache-status", ["DYNAMIC"]) not in (["DYNAMIC"], ["BYPASS"])
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


def _access_headers(access):
    client_id = access.get("SUBSONIC_HEALTH_ACCESS_CLIENT_ID", "")
    secret = access.get("SUBSONIC_HEALTH_ACCESS_CLIENT_SECRET", "")
    # Visible ASCII only: reject control characters and config/header injection.
    if any(not re.fullmatch(r"[!-~]{1,512}", value) for value in (client_id, secret)):
        raise UnsafeHealth("access-config")
    return f"CF-Access-Client-Id: {client_id}\nCF-Access-Client-Secret: {secret}\n".encode()


def _run_curl(run, args, timeout_ms, headers=None):
    try:
        return run(
            ["curl", "--disable", *args],
            stdin=subprocess.DEVNULL if headers is None else None,
            input=headers,
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
    expected_sha,
    *,
    deadline_ms,
    access=None,
    run=subprocess.run,
    monotonic=time.monotonic,
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
        "@-",
        "--header",
        "Accept-Encoding: identity",
        "--header",
        "Cache-Control: no-cache, no-store",
        "--write-out",
        "\x1e%{http_code}\x1e%{header_json}",
        f"{HEALTH_URL}?probe={secrets.token_hex(16)}",
    ]
    headers = _access_headers(os.environ if access is None else access)
    preflight_ms = monotonic() * 1000
    cutoff_ms = min(deadline_ms, int(preflight_ms) + MAX_SAMPLE_AGE_MS)
    if not preflight_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    _require_streaming_cap(_run_curl(run, ("--version",), cutoff_ms - preflight_ms))
    started_ms = monotonic() * 1000
    if not preflight_ms <= started_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    result = _run_curl(run, args, cutoff_ms - started_ms, headers)
    returned_ms = monotonic() * 1000
    if not started_ms <= returned_ms < cutoff_ms:
        raise UnsafeHealth("deadline")
    if result.returncode != 0:
        raise UnsafeHealth("transport")
    _validate(result.stdout, expected_sha)
    if not returned_ms <= monotonic() * 1000 < cutoff_ms:
        raise UnsafeHealth("deadline")
    return BuildHealth(int(started_ms), expected_sha)


if __name__ == "__main__":
    try:
        health = sample_build_health(
            os.environ.get("EXPECTED_STAGING_BUILD_SHA"),
            deadline_ms=int(time.monotonic() * 1000) + MAX_SAMPLE_AGE_MS,
        )
    except UnsafeHealth as exc:
        raise SystemExit(f"Staging build health unavailable: {exc}") from None
    print(
        f"Staging build health verified for {health.build_sha}; not measurement admission."
    )
