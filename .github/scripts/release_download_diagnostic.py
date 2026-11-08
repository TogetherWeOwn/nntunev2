"""Manual hosted-runner probe of original release bytes; never install or execute them."""

import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import tempfile
import time
from urllib.parse import urljoin, urlsplit

# Original nntune workflow pins and public-pr-security.yml at
# feb1e0e2111da007f38ca75ee95c6f6944723d36. No mirrors or alternative assets.
ASSETS = (
    ("gitleaks", "https://github.com/gitleaks/gitleaks/releases/download/v8.30.1/gitleaks_8.30.1_linux_x64.tar.gz",
     "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb"),
    ("jq", "https://github.com/jqlang/jq/releases/download/jq-1.8.2/jq-linux-amd64",
     "b1c22172dd303f3be49e935aa56aa48a8b7a46e0bc838b4997d3bb451495870f"),
    ("zizmor", "https://github.com/zizmorcore/zizmor/releases/download/v1.32.0/zizmor-x86_64-unknown-linux-gnu.tar.gz",
     "bb110cc765eca9cd7efb2e284a956be5974c60027b0970f16397d86208a9957b"),
    ("pinact", "https://github.com/suzuki-shunsuke/pinact/releases/download/v5.0.2/pinact_linux_amd64.tar.gz",
     "7f003d8f51e984c2264d2009d430e3e0e87ae404a0e2dc2a9a277b0f731bf5b7"),
)
HOSTS = {"github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"}
MAX_HOPS = 4
MAX_BYTES = 32 * 1024 * 1024
TIMINGS = ("status", "dns_s", "connect_s", "tls_s", "first_byte_s", "total_s")
SAFE_HEADERS = {
    "x-github-request-id": r"[A-Fa-f0-9:-]{1,100}",
    "x-ms-request-id": r"[A-Fa-f0-9-]{1,100}",
    "content-length": r"[0-9]{1,12}",
    "retry-after": r"[0-9]{1,8}",
    "x-cache": r"[A-Za-z ,_-]{1,100}",
    "server": r"(?:GitHub\.com|github\.com|envoy|Varnish|nginx|squid)",
}


def emit(**record):
    print(json.dumps(record, sort_keys=True), flush=True)


def host(url):
    parts = urlsplit(url)
    if (parts.scheme != "https" or parts.hostname not in HOSTS
            or parts.port not in (None, 443) or parts.username or parts.password
            or any(ord(c) < 33 or ord(c) > 126 or c in '\\"' for c in url)):
        raise ValueError("unapproved redirect")
    return parts.hostname


def request(url, output):
    # No -L, retries, auth, cookies, netrc, config files, body logs or verbose
    # tracing. Signed redirect URLs exist only in memory/stdin, never argv/logs.
    command = ["curl", "--disable", "--globoff", "--silent", "--fail", "--proto", "=https",
               "--connect-timeout", "10", "--max-time", "30",
               "--max-filesize", str(MAX_BYTES), "--dump-header", "-",
               "--output", str(output), "--write-out",
               "\nNNTUNE_TIMING %{http_code} %{time_namelookup} %{time_connect} "
               "%{time_appconnect} %{time_starttransfer} %{time_total}\n", "--config", "-"]
    result = subprocess.run(command, input=f'url = "{url}"\n'.encode(),
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=32)
    if len(result.stdout) > 65536:
        raise ValueError("oversized metadata")
    headers, marker, metrics = result.stdout.rpartition(b"\nNNTUNE_TIMING ")
    values = metrics.split()
    if not marker or len(values) != len(TIMINGS):
        raise ValueError("missing timing metadata")
    if not re.fullmatch(rb"[0-9]{3}", values[0]) or any(
            not re.fullmatch(rb"[0-9]{1,5}\.[0-9]{1,6}", v) for v in values[1:]):
        raise ValueError("invalid timing metadata")
    timing = dict(zip(TIMINGS, [int(values[0]), *(float(v) for v in values[1:])]))
    # A proxy CONNECT or interim response may precede the actual response.
    # Split only HTTP delimiters; preserve controls for redirect validation.
    blocks = [block for block in headers.split(b"\r\n\r\n") if block]
    fields = {}
    for line in blocks[-1].split(b"\r\n")[1:]:
        key, sep, value = line.partition(b":")
        if sep:
            fields[key.decode("ascii").lower()] = value.strip(b" ").decode("ascii")
    safe = {k: v for k, v in fields.items()
            if k in SAFE_HEADERS and re.fullmatch(SAFE_HEADERS[k], v)}
    return result.returncode, timing, safe, fields.get("location")


def probe(asset, temp_root):
    name, url, expected = asset
    passed = False
    directory = None
    current_host = None
    start = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix="release-download-", dir=temp_root) as scratch:
            directory = Path(scratch)
            if any(directory.iterdir()):
                raise ValueError("nonempty scratch directory")
            payload = directory / "payload"
            for hop in range(MAX_HOPS):
                current_host = host(url)
                rc, timing, headers, location = request(url, payload)
                emit(asset=name, hop=hop, host=current_host, curl_exit=rc,
                     headers=headers, **timing)
                if rc:
                    break
                if timing["status"] in (301, 302, 303, 307, 308):
                    # urljoin strips some control characters: validate raw input first.
                    if not location or any(ord(c) < 33 or ord(c) > 126 or c in '\\"' for c in location):
                        raise ValueError("invalid redirect")
                    url = urljoin(url, location)
                    host(url)
                    payload.unlink(missing_ok=True)
                    continue
                if timing["status"] == 200 and 0 < payload.stat().st_size <= MAX_BYTES:
                    with payload.open("rb") as stream:
                        actual = hashlib.file_digest(stream, "sha256").hexdigest()
                    passed = actual == expected
                    emit(asset=name, final_host=current_host, sha256=actual,
                         expected_sha256=expected, checksum="passed" if passed else "failed")
                break
    except Exception as error:
        # Exception text can contain signed URLs, headers or local paths.
        emit(asset=name, failure_type=type(error).__name__)
    finally:
        cleaned = directory is not None and not os.path.lexists(directory)
        emit(asset=name, final_host=current_host, cleanup="passed" if cleaned else "failed",
             elapsed_s=round(time.monotonic() - start, 3),
             result="passed" if passed and cleaned else "failed")
    return passed and cleaned


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("RUNNER_ENVIRONMENT") != "github-hosted":
        raise SystemExit("diagnostic requires a GitHub-hosted Actions runner")
    sha = os.environ.get("GITHUB_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise SystemExit("missing exact workflow SHA")
    # SIGTERM follows the same finally/context-manager cleanup as ordinary failure.
    def stop(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, stop)
    emit(workflow_sha=sha, diagnostic="download-only", attempts_per_hop=1)
    results = [probe(asset, os.environ["RUNNER_TEMP"]) for asset in ASSETS]
    emit(all_original_checksums_passed=all(results))
    return 0 if all(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
