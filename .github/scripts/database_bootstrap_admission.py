"""Admission for one manually authorized main run. This module performs no SQL."""

import base64
import hashlib
import json
import os
import re
import sys
import time

REPOSITORY = "togetherweown/nntunev2"
WORKFLOW = ".github/workflows/deploy-database-bootstrap.yml"
MAIN = "refs/heads/main"
MAX_EVENT_BYTES = 256 * 1024
MAX_PLAN_BYTES = 4096
MAX_WINDOW_SECONDS = 7200


def authorized_plan(env, event, now):
    sha = env.get("BOOTSTRAP_SHA", "")
    run_id = env.get("BOOTSTRAP_RUN_ID", "")
    expiry = env.get("BOOTSTRAP_EXPIRY", "")
    digest = env.get("BOOTSTRAP_PLAN_DIGEST", "")
    encoded = env.get("BOOTSTRAP_PLAN_B64", "")
    repo, separator, workflow_ref = env.get("GITHUB_WORKFLOW_REF", "").partition(f"/{WORKFLOW}@")
    if not (
        env.get("GITHUB_REPOSITORY", "").lower() == REPOSITORY
        and repo.lower() == REPOSITORY and separator and workflow_ref == MAIN
        and env.get("GITHUB_EVENT_NAME") == "workflow_dispatch"
        and env.get("GITHUB_REF") == MAIN
        and env.get("GITHUB_RUN_ATTEMPT") == "1"
        and env.get("RUNNER_DEBUG", "") == ""
        and env.get("BOOTSTRAP_RUNNER_DEBUG", "").lower() in ("", "false")
        and env.get("BOOTSTRAP_STEP_DEBUG", "").lower() in ("", "false")
        and isinstance(event, dict)
        and event.get("inputs") in (None, {})
        and re.fullmatch(r"[0-9a-f]{40}", sha)
        and env.get("GITHUB_SHA") == env.get("GITHUB_WORKFLOW_SHA") == sha
        and re.fullmatch(r"[1-9][0-9]{0,19}", run_id)
        and env.get("GITHUB_RUN_ID") == run_id
        and re.fullmatch(r"[0-9]{10}", expiry)
        and 0 < int(expiry) - now <= MAX_WINDOW_SECONDS
        and re.fullmatch(r"[0-9a-f]{64}", digest)
        and 0 < len(encoded) <= 4 * ((MAX_PLAN_BYTES + 2) // 3)
    ):
        return None
    try:
        plan = base64.b64decode(encoded, validate=True)
    except ValueError:
        return None
    if (
        0 < len(plan) <= MAX_PLAN_BYTES
        and base64.b64encode(plan).decode("ascii") == encoded
        and hashlib.sha256(plan).hexdigest() == digest
    ):
        return plan
    return None


def read_authorized_plan(env):
    if len(sys.argv) != 1:
        return None
    with open(env["GITHUB_EVENT_PATH"], "rb") as source:
        raw = source.read(MAX_EVENT_BYTES + 1)
    if len(raw) <= MAX_EVENT_BYTES:
        return authorized_plan(env, json.loads(raw), time.time())
    return None


if __name__ == "__main__":
    try:
        admitted = read_authorized_plan(os.environ)
    except Exception:
        admitted = False
    if not admitted:
        sys.exit("database-bootstrap-admission: refused")
    print("database-bootstrap-admission: authorized")
