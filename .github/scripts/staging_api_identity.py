"""Produce a redacted, point-in-time receipt for the staging API Worker."""

import http.client
import json
import os
import re
import sys
from decimal import Decimal, InvalidOperation
from pathlib import Path

SCRIPT = "nntune-api-staging"
HOST = "api.cloudflare.com"
MAX_BODY = 1024 * 1024
MAX_DEPLOY_OUTPUT = 64 * 1024
SHA = re.compile(r"[0-9a-f]{40}")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}")
PREFIX = "SUBSONIC_MEASUREMENT_"


class Unavailable(Exception):
    """Only raise with static reason codes, never provider data."""


def require(condition, reason):
    if not condition:
        raise Unavailable(reason)


def unique_object(pairs):
    result = dict(pairs)
    if len(result) != len(pairs):
        raise ValueError
    return result


def reject_constant(_):
    raise ValueError


def decode(body, reason):
    try:
        return json.loads(body.decode("utf-8"), object_pairs_hook=unique_object,
                          parse_constant=reject_constant, parse_float=Decimal)
    except (ValueError, RecursionError, InvalidOperation):
        raise Unavailable(reason) from None


def field(obj, name):
    return obj.get(name) if isinstance(obj, dict) else None


def valid(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def deployed_version(body):
    # The action has no version-ID output; use Wrangler 4.162.0's session and deploy v1 JSONL.
    reason = "deploy-output-unavailable"
    require(len(body) <= MAX_DEPLOY_OUTPUT and body.endswith(b"\n"), reason)
    records = [decode(line, reason) for line in body.splitlines()]
    require(all(field(record, "type") in ("wrangler-session", "deploy") for record in records), reason)
    deploys = [record for record in records if record["type"] == "deploy"]
    require(len(deploys) == 1, reason)
    deploy = deploys[0]
    version_id = deploy.get("version_id")
    require(type(deploy.get("version")) is int and deploy["version"] == 1, reason)
    require(deploy.get("worker_name") == SCRIPT and deploy.get("wrangler_environment") == "staging", reason)
    require(valid(UUID, version_id), reason)
    return version_id


def read_chunked(stream):
    # Require complete chunk framing; do not accept extensions or trailers.
    body = bytearray()
    while True:
        line = stream.readline(19)
        require(re.fullmatch(rb"[0-9a-fA-F]{1,16}\r\n", line), "provider-transport")
        size = int(line, 16)
        if size == 0:
            require(stream.read(2) == b"\r\n", "provider-transport")
            return body
        require(len(body) + size <= MAX_BODY, "provider-body-limit")
        chunk = stream.read(size)
        require(len(chunk) == size and stream.read(2) == b"\r\n", "provider-transport")
        body.extend(chunk)


def read_response(stream, endpoint):
    # Validate raw HTTP syntax before any parser can normalize malformed lines.
    reason = "provider-transport"
    status = re.fullmatch(rb"HTTP/1\.([01]) ([0-9]{3}) [\t\x20-\x7e\x80-\xff]*\r\n", stream.readline(8192))
    require(status, reason)
    minor, code = status.groups()
    if code in (b"401", b"403"):
        raise Unavailable(endpoint + "-auth-denied")
    require(code == b"200", "provider-http-status")
    framing = {}
    for _ in range(100):
        line = stream.readline(8192)
        if line == b"\r\n":
            break
        header = re.fullmatch(rb"([!#$%&'*+.^_`|~0-9A-Za-z-]+):([\t\x20-\x7e\x80-\xff]*)\r\n", line)
        require(header, reason)
        name, value = header.groups()
        name = name.lower()
        if name in (b"transfer-encoding", b"content-length"):
            require(name not in framing, reason)
            framing[name] = value.strip(b" \t").lower()
    else:
        raise Unavailable(reason)
    if b"transfer-encoding" in framing:
        require(minor == b"1" and framing == {b"transfer-encoding": b"chunked"}, reason)
        return read_chunked(stream)
    length = framing.get(b"content-length", b"")
    require(re.fullmatch(rb"[0-9]+", length), reason)
    size = int(length)
    require(size <= MAX_BODY, "provider-body-limit")
    body = stream.read(size)
    require(len(body) == size, reason)
    return body


def provider_result(path, token):
    # Fixed TLS origin, GET only, no redirects, proxy, retries or response logs.
    conn = http.client.HTTPSConnection(HOST, timeout=5)
    try:
        conn.request("GET", path, headers={"Authorization": f"Bearer {token}", "Cache-Control": "no-cache, no-store"})
        with conn.sock.makefile("rb") as stream:
            body = read_response(stream, "deployments" if path.endswith("/deployments") else "version")
    except (http.client.HTTPException, OSError, ValueError):
        raise Unavailable("provider-transport") from None
    finally:
        conn.close()
    payload = decode(body, "provider-response-shape")
    result = field(payload, "result")
    ok = field(payload, "success") is True and field(payload, "errors") == []
    require(ok and isinstance(result, dict), "provider-response-shape")
    return result


def current_deployment(result, version_id):
    # As in Wrangler, Cloudflare lists the latest serving deployment first, not the latest version upload.
    deployments = field(result, "deployments")
    require(isinstance(deployments, list) and deployments, "deployment-shape")
    ids = [field(deployment, "id") for deployment in deployments]
    require(all(valid(UUID, value) for value in ids) and len(set(ids)) == len(ids), "deployment-shape")
    deployment = deployments[0]
    versions = field(deployment, "versions")
    single = deployment.get("strategy") == "percentage" and isinstance(versions, list) and len(versions) == 1
    require(single, "deployment-not-single-version")
    percent, active_id = field(versions[0], "percentage"), field(versions[0], "version_id")
    require(type(percent) in (int, Decimal) and percent == 100 and valid(UUID, active_id), "deployment-traffic-shape")
    require(active_id == version_id, "deployed-version-mismatch")
    return ids[0]


def verify_version(result, version_id, expected_sha):
    require(field(result, "id") == version_id, "provider-version-mismatch")
    bindings = field(field(result, "resources"), "bindings")
    require(isinstance(bindings, list), "version-bindings-unavailable")
    by_name = {}
    for binding in bindings:
        name, kind = field(binding, "name"), field(binding, "type")
        require(isinstance(name, str) and name and isinstance(kind, str) and kind and name not in by_name,
                "version-bindings-shape")
        by_name[name] = binding
    build = by_name.get("BUILD_SHA")
    require(field(build, "type") == "plain_text" and valid(SHA, field(build, "text")), "version-build-unavailable")
    require(build["text"] == expected_sha, "version-build-mismatch")
    enable = by_name.get(PREFIX + "ENABLE")
    off = enable is None or (enable["type"] == "plain_text" and enable.get("text") == "0")
    require(off and PREFIX + "LEASE" not in by_name and PREFIX + "DB" not in by_name, "default-off-not-verified")


def receipt(body, expected_sha, account, read):
    require(valid(SHA, expected_sha), "expected-build-shape")
    require(valid(re.compile(r"[0-9a-f]{32}"), account), "account-config")
    version_id = deployed_version(body)
    path = f"/client/v4/accounts/{account}/workers/scripts/{SCRIPT}"
    deployments = path + "/deployments"
    deployment_id = current_deployment(read(deployments), version_id)
    verify_version(read(path + "/versions/" + version_id), version_id, expected_sha)
    require(current_deployment(read(deployments), version_id) == deployment_id, "deployment-changed-during-readback")
    return matched_receipt(deployment_id, version_id, expected_sha)


def matched_receipt(deployment_id, version_id, build_sha):
    return {"identity": "matched", "deployment_id": deployment_id, "version_id": version_id,
            "build_sha": build_sha, "enable_flag": "off", "lease": "absent", "measurement_db_binding": "absent"}


# Only static producer codes may cross the report boundary; new codes fail closed.
REPORTABLE_REASONS = (
    "account-config", "credential-config", "default-off-not-verified", "deploy-output-unavailable",
    "deployed-version-mismatch", "deployment-changed-during-readback", "deployment-not-single-version",
    "deployment-shape", "deployment-traffic-shape", "deployments-auth-denied", "expected-build-shape",
    "provider-body-limit", "provider-http-status", "provider-response-shape", "provider-transport",
    "provider-version-mismatch", "version-auth-denied", "version-bindings-shape",
    "version-bindings-unavailable", "version-build-mismatch", "version-build-unavailable",
)


def report_receipt(path, expected_sha):
    """Validate and re-encode a saved receipt without consulting the provider."""
    limit = 512  # Both fixed receipt shapes fit comfortably in this bound.
    try:
        with Path(path).open("rb") as source:
            body = source.read(limit + 1)
        if len(body) > limit:
            return 1
        saved = decode(body, "invalid-receipt")
    except (OSError, Unavailable):
        return 1
    if field(saved, "identity") == "matched":
        deployment_id, version_id = saved.get("deployment_id"), saved.get("version_id")
        if not (valid(UUID, deployment_id) and valid(UUID, version_id) and valid(SHA, expected_sha)):
            return 1
        redacted = matched_receipt(deployment_id, version_id, expected_sha)
    elif field(saved, "identity") == "unavailable" and saved.get("reason") in REPORTABLE_REASONS:
        redacted = {"identity": "unavailable", "reason": saved["reason"]}
    else:
        return 1
    if saved != redacted:
        return 1
    print(json.dumps(redacted, separators=(",", ":")))
    return 0


def main():
    try:
        token = os.environ.get("CLOUDFLARE_API_TOKEN", "")
        require(valid(re.compile(r"[\x21-\x7e]{1,512}"), token), "credential-config")
        try:
            (output,) = Path(os.environ["WRANGLER_OUTPUT_FILE_DIRECTORY"]).glob("wrangler-output-*.json")
            with output.open("rb") as source:
                body = source.read(MAX_DEPLOY_OUTPUT + 1)
        except (OSError, KeyError, ValueError):
            raise Unavailable("deploy-output-unavailable") from None
        result = receipt(body, os.getenv("GITHUB_SHA"), os.getenv("CLOUDFLARE_ACCOUNT_ID"),
                         lambda path: provider_result(path, token))
    except Unavailable as exc:
        print(json.dumps({"identity": "unavailable", "reason": str(exc)}))
        return 1
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    if len(sys.argv) == 1:
        raise SystemExit(main())
    if len(sys.argv) == 4 and sys.argv[1] == "--validate-receipt":
        raise SystemExit(report_receipt(sys.argv[2], sys.argv[3]))
    raise SystemExit(2)
