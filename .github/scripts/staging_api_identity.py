"""Produce a redacted, point-in-time receipt for the staging API Worker."""

import http.client
import json
import os
import re
import signal
import sys
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
        return json.loads(
            body.decode("utf-8"), object_pairs_hook=unique_object, parse_constant=reject_constant
        )
    except (ValueError, RecursionError):
        raise Unavailable(reason) from None


def field(obj, name):
    return obj.get(name) if isinstance(obj, dict) else None


def valid(pattern, value):
    return isinstance(value, str) and pattern.fullmatch(value) is not None


def deployed_version(body):
    # The pinned action has no version-ID output. Wrangler 4.162.0 writes
    # wrangler-session and deploy v1 JSONL to WRANGLER_OUTPUT_FILE_PATH.
    reason = "deploy-output-unavailable"
    require(len(body) <= MAX_DEPLOY_OUTPUT and body.endswith(b"\n"), reason)
    records = [decode(line, reason) for line in body.splitlines()]
    require(all(field(record, "type") in ("wrangler-session", "deploy") for record in records), reason)
    deploys = [record for record in records if record["type"] == "deploy"]
    require(len(deploys) == 1, reason)
    deploy = deploys[0]
    version_id = deploy.get("version_id")
    require(
        type(deploy.get("version")) is int
        and deploy["version"] == 1
        and deploy.get("worker_name") == SCRIPT
        and deploy.get("wrangler_environment") == "staging"
        and valid(UUID, version_id),
        reason,
    )
    return version_id


def provider_result(path, token):
    # Fixed TLS origin, GET only, no redirects, proxy, retries or response logs.
    conn = http.client.HTTPSConnection(HOST, timeout=5)
    try:
        conn.request(
            "GET",
            path,
            headers={"Authorization": f"Bearer {token}", "Cache-Control": "no-cache, no-store"},
        )
        with conn.getresponse() as response:
            if response.status in (401, 403):
                raise Unavailable("provider-auth-denied")
            require(response.status == 200, "provider-http-status")
            body = response.read(MAX_BODY + 1)
    except (http.client.HTTPException, OSError, ValueError):
        raise Unavailable("provider-transport") from None
    finally:
        conn.close()
    require(len(body) <= MAX_BODY, "provider-body-limit")
    payload = decode(body, "provider-response-shape")
    require(
        field(payload, "success") is True
        and field(payload, "errors") == []
        and isinstance(field(payload, "result"), dict),
        "provider-response-shape",
    )
    return payload["result"]


def current_deployment(result, version_id):
    # Cloudflare's list contract (also used by `wrangler deployments status`):
    # the first entry is the latest deployment serving traffic, not the latest upload.
    deployments = field(result, "deployments")
    require(isinstance(deployments, list) and deployments, "deployment-shape")
    ids = [field(deployment, "id") for deployment in deployments]
    require(all(valid(UUID, value) for value in ids) and len(set(ids)) == len(ids), "deployment-shape")
    deployment = deployments[0]
    versions = field(deployment, "versions")
    require(
        deployment.get("strategy") == "percentage" and isinstance(versions, list) and len(versions) == 1,
        "deployment-not-single-version",
    )
    active = versions[0]
    percent = field(active, "percentage")
    active_id = field(active, "version_id")
    require(
        type(percent) in (int, float) and percent == 100 and valid(UUID, active_id),
        "deployment-traffic-shape",
    )
    require(active_id == version_id, "deployed-version-mismatch")
    return ids[0]


def verify_version(result, version_id, expected_sha):
    require(field(result, "id") == version_id, "provider-version-mismatch")
    bindings = field(field(result, "resources"), "bindings")
    require(isinstance(bindings, list), "version-bindings-unavailable")
    by_name = {}
    for binding in bindings:
        name, kind = field(binding, "name"), field(binding, "type")
        require(
            isinstance(name, str) and name and isinstance(kind, str) and kind and name not in by_name,
            "version-bindings-shape",
        )
        by_name[name] = binding
    build = by_name.get("BUILD_SHA")
    require(
        field(build, "type") == "plain_text" and valid(SHA, field(build, "text")),
        "version-build-unavailable",
    )
    require(build["text"] == expected_sha, "version-build-mismatch")
    enable = by_name.get(PREFIX + "ENABLE")
    require(
        (enable is None or (enable["type"] == "plain_text" and enable.get("text") == "0"))
        and PREFIX + "LEASE" not in by_name
        and PREFIX + "DB" not in by_name,
        "default-off-not-verified",
    )


def receipt(body, expected_sha, account, read):
    require(valid(SHA, expected_sha), "expected-build-shape")
    require(valid(re.compile(r"[0-9a-f]{32}"), account), "account-config")
    version_id = deployed_version(body)
    path = f"/client/v4/accounts/{account}/workers/scripts/{SCRIPT}"
    deployments = path + "/deployments"
    deployment_id = current_deployment(read(deployments), version_id)
    verify_version(read(path + "/versions/" + version_id), version_id, expected_sha)
    require(
        current_deployment(read(deployments), version_id) == deployment_id,
        "deployment-changed-during-readback",
    )
    return {
        "identity": "matched",
        "deployment_id": deployment_id,
        "version_id": version_id,
        "build_sha": expected_sha,
        "enable_flag": "off",
        "lease": "absent",
        "measurement_db_binding": "absent",
    }


def deadline(_signum, _frame):
    raise Unavailable("deadline")


def main():
    signal.signal(signal.SIGALRM, deadline)
    signal.alarm(25)
    try:
        token = os.environ.get("CLOUDFLARE_API_TOKEN", "")
        require(valid(re.compile(r"[\x21-\x7e]{1,512}"), token), "credential-config")
        try:
            with Path(os.environ.get("WRANGLER_OUTPUT_FILE_PATH", "")).open("rb") as source:
                body = source.read(MAX_DEPLOY_OUTPUT + 1)
        except (OSError, ValueError):
            raise Unavailable("deploy-output-unavailable") from None
        result = receipt(
            body,
            os.environ.get("EXPECTED_BUILD_SHA", ""),
            os.environ.get("CLOUDFLARE_ACCOUNT_ID", ""),
            lambda path: provider_result(path, token),
        )
    except Unavailable as exc:
        print(json.dumps({"identity": "unavailable", "reason": str(exc)}))
        return 1
    finally:
        signal.alarm(0)
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
