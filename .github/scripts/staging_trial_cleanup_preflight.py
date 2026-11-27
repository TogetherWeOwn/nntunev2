"""Recheck the pinned disable deployment under the existing staging deploy lock."""

import json
import os
import re
import sys

import staging_api_identity as identity
from staging_stream_config import API_SETTINGS, PRIVATE_SETTINGS, TRIAL_DATABASES

SHA256 = re.compile(r"[0-9a-f]{64}")


def preflight(env, read):
    require = identity.require
    for name in ("PRIVATE_TRIAL_UPLOAD_STAGING_ENABLED", "PRIVATE_PLAYBACK_STAGING_ENABLED",
                 "PRIVATE_MUSIC_STAGING_JOURNEY", "PRIVATE_PLAYBACK_STAGING_READ_OBSERVATION",
                 "STREAM_STAGING_DEPLOY_ENABLED"):
        require(not env.get(name), "staging-arms-present")
    scope_sha = env.get("CLEANUP_SCOPE_SHA256", "")
    require(identity.valid(SHA256, scope_sha), "missing-scope-pin")
    raw = env.get("CLEANUP_CONTAINMENT", "")
    require(0 < len(raw) <= 2048, "missing-containment")
    receipt = identity.decode(raw.encode(), "invalid-containment")
    require(isinstance(receipt, dict) and set(receipt) == {
        "session_revoked", "cohort_cleared", "armed_runs_drained", "build_sha", "api", "stream",
        "cleanup_scope_sha256", "cleanup_s3_access_key_sha256",
    } and all(receipt[k] is True for k in ("session_revoked", "cohort_cleared", "armed_runs_drained")),
        "invalid-containment")
    require(receipt["cleanup_scope_sha256"] == scope_sha, "cleanup-scope-drift")
    access_key_sha = receipt["cleanup_s3_access_key_sha256"]
    require(identity.valid(SHA256, access_key_sha), "invalid-cleanup-access-key-pin")
    sha = receipt["build_sha"]
    require(identity.valid(identity.SHA, sha), "invalid-disable-build")
    provider = {}
    for field, setting in {
        "environment": "ENVIRONMENT", "library": "LIBRARY", "configuration": "S3_CONFIGURATION",
        "root_version": "ROOT_VERSION", "bucket": "S3_BUCKET", "endpoint": "S3_ENDPOINT", "region": "S3_REGION",
    }.items():
        value = env.get("PRIVATE_PLAYBACK_STAGING_" + setting, "")
        require(0 < len(value) <= 253 and re.fullmatch((API_SETTINGS | PRIVATE_SETTINGS)[setting], value),
                "invalid-provider-setting")
        provider[field] = value
    lanes = (("api", identity.SCRIPT), ("stream", identity.STREAM))
    for lane, _ in lanes:
        pin = receipt[lane]
        require(isinstance(pin, dict) and set(pin) == {"deployment_id", "version_id"} and
                all(identity.valid(identity.UUID, value) for value in pin.values()), "invalid-disable-pin")
    def check_deployments():
        for lane, worker in lanes:
            pin = receipt[lane]
            deployed = identity.current_deployment(read(worker, "deployments"), pin["version_id"])
            require(deployed == pin["deployment_id"], "disable-deployment-drift")

    check_deployments()
    for lane, worker in lanes:
        pin = receipt[lane]
        bindings = identity.verify_build(read(worker, "versions/" + pin["version_id"]), pin["version_id"], sha)
        mode = bindings.get("PRIVATE_PLAYBACK_ENABLED")
        off = {"type": "plain_text", "text": "false"}
        require((lane == "api" and mode is None) or
                (isinstance(mode, dict) and all(mode.get(k) == v for k, v in off.items())),
                "playback-not-disabled")
        if lane == "api":
            require("PRIVATE_TRIAL_UPLOAD_ENABLED" not in bindings and
                    all(f"PRIVATE_TRIAL_{role}_DB" not in bindings for role in TRIAL_DATABASES),
                    "upload-not-disabled")
    check_deployments()

    return {"provider": provider, "scope_sha256": scope_sha, "s3_access_key_sha256": access_key_sha}


if __name__ == "__main__":
    try:
        account = os.environ.get("CLOUDFLARE_ACCOUNT_ID", "")
        token = os.environ.get("CLOUDFLARE_WORKERS_READ_TOKEN", "")
        identity.require(re.fullmatch(r"[0-9a-f]{32}", account) and re.fullmatch(r"[!-~]{1,512}", token),
                         "missing-read-authority")
        prefix = f"/client/v4/accounts/{account}/workers/scripts"
        approval = preflight(os.environ, lambda worker, path: identity.provider_result(f"{prefix}/{worker}/{path}", token))
        print(json.dumps(approval, separators=(",", ":")))
    except (identity.Unavailable, OSError, ValueError, KeyError, RecursionError, TypeError, AttributeError):
        sys.exit("Private staging fixture cleanup containment refused.")
