"""Check baseline workflow gates and exercise admission with generated inputs."""

import base64
import hashlib
import json
import secrets
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import database_bootstrap_admission as admission
from database_bootstrap_baseline import WORKFLOW as BASELINE_WORKFLOW
from test_ci_pnpm_retry import job_needs, split_jobs, step_field


def check_baseline_workflow():
    root = Path(__file__).resolve().parents[2]
    workflow = (root / BASELINE_WORKFLOW).read_text(encoding="utf-8")
    jobs = split_jobs(workflow)
    preflight = jobs["preflight"].split("\n    steps:\n", 1)[0]
    baseline = jobs["baseline"].split("\n    steps:\n", 1)[0]
    gate = """    if: >-
      ${{ github.repository == 'TogetherWeOwn/nntunev2' &&
          github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/main' &&
          github.run_attempt == '1' && vars.DATABASE_BASELINE_CANDIDATE_SHA == github.sha }}
"""
    if (
        "\non:\n  workflow_dispatch:\n\n" not in workflow or gate not in preflight
        or job_needs(baseline) != ["preflight"]
        or step_field(baseline, "if") != "github.run_attempt == '1'"
        or step_field(baseline, "environment") != "production-db-baseline"
    ):
        raise RuntimeError("baseline workflow admission differs from the required guards")


def exercise(event_path):
    plan = secrets.token_bytes(64)
    sha, run = secrets.token_hex(20), str(secrets.randbelow(10**10) + 1)
    env = {
        "GITHUB_REPOSITORY": admission.REPOSITORY,
        "GITHUB_WORKFLOW_REF": f"{admission.REPOSITORY}/{admission.WORKFLOW}@{admission.MAIN}",
        "GITHUB_EVENT_NAME": "workflow_dispatch",
        "GITHUB_REF": admission.MAIN,
        "GITHUB_SHA": sha,
        "GITHUB_WORKFLOW_SHA": sha,
        "GITHUB_RUN_ID": run,
        "GITHUB_RUN_ATTEMPT": "1",
        "GITHUB_EVENT_PATH": str(event_path),
        "BOOTSTRAP_SHA": sha,
        "BOOTSTRAP_RUN_ID": run,
        "BOOTSTRAP_EXPIRY": str(int(time.time()) + 60),
        "BOOTSTRAP_PLAN_B64": base64.b64encode(plan).decode("ascii"),
        "BOOTSTRAP_PLAN_DIGEST": hashlib.sha256(plan).hexdigest(),
    }

    def check(admit, event=b'{"inputs":{}}', args=(), **overrides):
        event_path.write_bytes(event)
        result = subprocess.run(
            [sys.executable, "-IB", admission.__file__, *args],
            env=env | overrides, stdin=subprocess.DEVNULL, capture_output=True, timeout=3, check=False,
        )
        expected = ((0, b"database-bootstrap-admission: authorized\n", b"") if admit else
                    (1, b"", b"database-bootstrap-admission: refused\n"))
        if (result.returncode, result.stdout, result.stderr) != expected:
            raise RuntimeError

    check(True)
    check(True, event=b'{}')
    for key, value in {
        "GITHUB_REPOSITORY": "fixture/fork",
        "GITHUB_WORKFLOW_REF": f"{admission.REPOSITORY}/other.yml@{admission.MAIN}",
        "GITHUB_EVENT_NAME": "pull_request_target",
        "GITHUB_REF": "refs/tags/main",
        "GITHUB_SHA": secrets.token_hex(20),
        "GITHUB_WORKFLOW_SHA": secrets.token_hex(20),
        "GITHUB_RUN_ID": str(int(run) + 1),
        "GITHUB_RUN_ATTEMPT": "2",
        "BOOTSTRAP_SHA": "",
        "BOOTSTRAP_RUN_ID": "",
        "BOOTSTRAP_EXPIRY": str(int(time.time()) - 1),
        "BOOTSTRAP_PLAN_B64": "!" * 100,
        "BOOTSTRAP_PLAN_DIGEST": secrets.token_hex(32),
        "RUNNER_DEBUG": "1",
        "BOOTSTRAP_RUNNER_DEBUG": "true",
        "BOOTSTRAP_STEP_DEBUG": "True",
    }.items():
        check(False, **{key: value})

    check(False, GITHUB_REF="refs/heads/topic")
    check(False, GITHUB_WORKFLOW_REF=f"{admission.REPOSITORY}/{admission.WORKFLOW}@refs/heads/topic")
    check(False, GITHUB_WORKFLOW_REF=f"fixture/fork/{admission.WORKFLOW}@{admission.MAIN}")
    check(False, BOOTSTRAP_EXPIRY=str(int(time.time()) + admission.MAX_WINDOW_SECONDS + 60))
    check(False, BOOTSTRAP_PLAN_B64="AB==", BOOTSTRAP_PLAN_DIGEST=hashlib.sha256(b"\0").hexdigest())
    oversized = secrets.token_bytes(admission.MAX_PLAN_BYTES + 1)
    check(False, BOOTSTRAP_PLAN_B64=base64.b64encode(oversized).decode("ascii"),
          BOOTSTRAP_PLAN_DIGEST=hashlib.sha256(oversized).hexdigest())
    check(False, BOOTSTRAP_PLAN_B64=base64.b64encode(oversized + plan).decode("ascii"),
          BOOTSTRAP_PLAN_DIGEST=hashlib.sha256(oversized + plan).hexdigest())
    check(False, args=(secrets.token_hex(20),))
    check(False, event=json.dumps({"inputs": {"value": secrets.token_hex(20)}}).encode())
    check(False, event=b'{"inputs":[]}')
    check(False, event=b"{")
    check(False, event=b" " * admission.MAX_EVENT_BYTES + b"{}")


if __name__ == "__main__":
    try:
        check_baseline_workflow()
        with tempfile.TemporaryDirectory(prefix="bootstrap-admission-") as temp:
            exercise(Path(temp) / "event.json")
        if Path(temp).exists():
            raise RuntimeError
    except Exception:
        sys.exit("database-bootstrap-admission-fixture: failed")
    print("database-bootstrap-admission-fixture: run binding, refusals and teardown passed")
