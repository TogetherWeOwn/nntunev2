"""Exercise the admission command with generated authorization and event data."""

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
    check(False, BOOTSTRAP_EXPIRY=str(int(time.time()) + admission.MAX_WINDOW_SECONDS + 60))
    check(False, args=(secrets.token_hex(20),))
    check(False, event=json.dumps({"inputs": {"value": secrets.token_hex(20)}}).encode())
    check(False, event=b'{"inputs":[]}')
    check(False, event=b"{")
    check(False, event=b" " * admission.MAX_EVENT_BYTES + b"{}")


if __name__ == "__main__":
    try:
        with tempfile.TemporaryDirectory(prefix="bootstrap-admission-") as temp:
            exercise(Path(temp) / "event.json")
        if Path(temp).exists():
            raise RuntimeError
    except Exception:
        sys.exit("database-bootstrap-admission-fixture: failed")
    print("database-bootstrap-admission-fixture: run binding, refusals and teardown passed")
