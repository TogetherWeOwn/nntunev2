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
from test_ci_pnpm_retry import job_level_field, job_needs, split_jobs


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
        or job_level_field(baseline, "if") != "github.run_attempt == '1'"
        or job_level_field(baseline, "environment") != "production-db-baseline"
    ):
        raise RuntimeError("baseline workflow admission differs from the required guards")


def synthetic_authorization(plan, sha, run, expiry, *, workflow=admission.WORKFLOW):
    return {
        "GITHUB_REPOSITORY": admission.REPOSITORY,
        "GITHUB_WORKFLOW_REF": f"{admission.REPOSITORY}/{workflow}@{admission.MAIN}",
        "GITHUB_EVENT_NAME": "workflow_dispatch",
        "GITHUB_REF": admission.MAIN,
        "GITHUB_SHA": sha,
        "GITHUB_WORKFLOW_SHA": sha,
        "GITHUB_RUN_ID": run,
        "GITHUB_RUN_ATTEMPT": "1",
        "BOOTSTRAP_SHA": sha,
        "BOOTSTRAP_RUN_ID": run,
        "BOOTSTRAP_EXPIRY": str(expiry),
        "BOOTSTRAP_PLAN_B64": base64.b64encode(plan).decode("ascii"),
        "BOOTSTRAP_PLAN_DIGEST": hashlib.sha256(plan).hexdigest(),
    }


def check_aborted_baseline_run_binding():
    now = 1800000000
    sha = secrets.token_hex(20)
    old_plan, fresh_plan = secrets.token_bytes(64), secrets.token_bytes(64)
    original = synthetic_authorization(old_plan, sha, "101", now + 60, workflow=BASELINE_WORKFLOW)
    fresh = synthetic_authorization(fresh_plan, sha, "102", now + 60, workflow=BASELINE_WORKFLOW)
    cases = (
        ("original first attempt before cancellation", original, old_plan),
        ("rerun after cancellation", original | {"GITHUB_RUN_ATTEMPT": "2"}, None),
        ("fresh dispatch using the canceled run authorization", original | {"GITHUB_RUN_ID": "102"}, None),
        ("fresh dispatch with its own authorization", fresh, fresh_plan),
    )
    for case, env, expected in cases:
        if admission.authorized_plan(env, {}, now, workflow=BASELINE_WORKFLOW) != expected:
            raise RuntimeError(f"baseline admission fixture: {case}")


def exercise(event_path):
    plan = secrets.token_bytes(64)
    sha, run = secrets.token_hex(20), str(secrets.randbelow(10**10) + 1)
    env = synthetic_authorization(plan, sha, run, int(time.time()) + 60) | {"GITHUB_EVENT_PATH": str(event_path)}

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


def snapshot_record(branch="fixture001"):
    at = 1800000000
    review = {
        "phase": "pre-bootstrap", "branch_id": branch,
        "protected_write_at": at + 120, "window_end_at": at + 600,
    }
    snapshot = {
        "kind": "ordinary-native", "branch_id": branch, "restore_scope": "whole-branch",
        "status": "succeeded", "captured_at": at, "completed_at": at + 30,
        "readback_at": at + 60, "expires_at": at + 3600,
    }
    return {"review": review, "snapshot": snapshot}


def check_snapshot(case, evidence, accept=False, *, args=()):
    script = Path(__file__).with_name("database_bootstrap_snapshot.py")
    raw = evidence if isinstance(evidence, bytes) else json.dumps(evidence).encode()
    result = subprocess.run(
        [sys.executable, "-IB", str(script), *args], input=raw,
        capture_output=True, timeout=3, check=False,
    )
    expected = ((0, b"database-bootstrap-snapshot: consistent\n", b"") if accept else
                (1, b"", b"database-bootstrap-snapshot: refused\n"))
    if (result.returncode, result.stdout, result.stderr) != expected:
        raise RuntimeError(f"snapshot fixture: {case}")


def check_snapshot_readback():
    record = snapshot_record()
    review, snapshot = record["review"], record["snapshot"]
    at = snapshot["captured_at"]
    check = check_snapshot

    check("pre-bootstrap", record, accept=True)
    for case, override in {
        "wrong branch": {"branch_id": "fixture002"},
        "wrong restore scope": {"restore_scope": "logical-database"},
        "different snapshot kind": {"kind": "manual"},
        "incomplete": {"status": "running"},
        "capture at write": {"captured_at": review["protected_write_at"],
                             "completed_at": review["protected_write_at"],
                             "readback_at": review["protected_write_at"]},
        "completion before capture": {"completed_at": at - 1},
        "readback before completion": {"readback_at": snapshot["completed_at"] - 1},
        "readback at write": {"readback_at": review["protected_write_at"]},
        "expired at write": {"expires_at": review["protected_write_at"]},
        "expires during window": {"expires_at": review["window_end_at"] - 1},
        "expires at window end": {"expires_at": review["window_end_at"]},
        "non-integer timestamp": {"captured_at": str(at)},
    }.items():
        check(case, record | {"snapshot": snapshot | override})
    check("missing readback", {"review": review})
    check("missing completion", record | {
        "snapshot": {key: value for key, value in snapshot.items() if key != "completed_at"},
    })
    check("window ends before write", record | {"review": review | {"window_end_at": at}})
    check("pre-bootstrap with setup completion", record | {"review": review | {"setup_completed_at": at - 1}})
    migration = review | {"phase": "pre-migration", "setup_completed_at": at - 1}
    check("post-setup pre-migration", record | {"review": migration}, accept=True)
    check("pre-migration without setup completion", record | {"review": review | {"phase": "pre-migration"}})
    check("pre-migration capture before setup", record | {"review": migration | {"setup_completed_at": at + 1}})
    check("pre-migration capture at setup", record | {"review": migration | {"setup_completed_at": at}})
    check("duplicate record key", b'{"snapshot": null, ' + json.dumps(record).encode()[1:])
    at_limit = json.dumps(record).encode().ljust(4096)
    check("record at size limit", at_limit, accept=True)
    check("record one byte over size limit", at_limit + b" ")
    check("malformed record", b"{")


def check_handoff_snapshot_branch(baseline_branch="fixture001"):
    record = snapshot_record(baseline_branch)
    baseline_pin = {"baseline_branch_id": baseline_branch}
    args = ("--baseline-handoff",)
    check_snapshot("pre-bootstrap evidence for post-setup handoff", record | baseline_pin, args=args)
    review = record["review"] | {"phase": "pre-migration"}
    check_snapshot("setup completion not recorded", record | {"review": review} | baseline_pin, args=args)

    completed = {"setup_completed_at": record["snapshot"]["captured_at"] - 1}
    post_setup = record | {"review": review | completed}
    other_branch = baseline_branch[:-1] + ("1" if baseline_branch.endswith("0") else "0")
    wrong_branch = {
        "review": post_setup["review"] | {"branch_id": other_branch},
        "snapshot": post_setup["snapshot"] | {"branch_id": other_branch},
    }
    check_snapshot("standalone snapshot for a different branch", wrong_branch, accept=True)
    check_snapshot("handoff snapshot for a different branch", wrong_branch | baseline_pin, args=args)
    check_snapshot("handoff without the exact baseline branch", post_setup, args=args)
    check_snapshot("retry with matching completed evidence", post_setup | baseline_pin, accept=True, args=args)


if __name__ == "__main__":
    try:
        check_baseline_workflow()
        check_aborted_baseline_run_binding()
        with tempfile.TemporaryDirectory(prefix="bootstrap-admission-") as temp:
            exercise(Path(temp) / "event.json")
        if Path(temp).exists():
            raise RuntimeError
        check_snapshot_readback()
    except Exception:
        sys.exit("database-bootstrap-admission-fixture: failed")
    print("database-bootstrap-admission-fixture: run binding, snapshot readback, refusals and teardown passed")
