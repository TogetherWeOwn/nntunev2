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


def check_reader_cleanup_binding():
    script = Path(__file__).with_name("database_bootstrap_reader_cleanup.py")
    reviewed = {
        "branch_id": "fixture001", "launch": "fixture_launch", "database_oid": 41001,
        "reader_role": "fixture_reader", "reader_oid": 42001,
    }
    receipt = reviewed | {"status": "absent"}
    record = {"reviewed": reviewed, "receipt": receipt}

    def check(case, evidence, *, accept=False):
        raw = evidence if isinstance(evidence, bytes) else json.dumps(evidence).encode()
        result = subprocess.run(
            [sys.executable, "-EsB", str(script)], input=raw,
            capture_output=True, timeout=3, check=False,
        )
        expected = ((0, b"database-bootstrap-reader-cleanup: consistent\n", b"") if accept else
                    (1, b"", b"database-bootstrap-reader-cleanup: pending\n"))
        if (result.returncode, result.stdout, result.stderr) != expected:
            raise RuntimeError(f"reader cleanup fixture: {case}")

    for key, other in {
        "branch_id": "fixture002", "launch": "fixture_other", "database_oid": 41002,
        "reader_role": "fixture_other_reader", "reader_oid": 42002,
    }.items():
        other_receipt = receipt | {key: other}
        check(f"receipt mismatches {key}", record | {"receipt": other_receipt})
        check(f"receipt valid for its own {key}",
              {"reviewed": reviewed | {key: other}, "receipt": other_receipt}, accept=True)

    for case, changes in {
        "zero database OID": {"database_oid": 0},
        "reader OID exceeds PostgreSQL range": {"reader_oid": 4294967296},
        "reserved database": {"launch": "template1"},
        "invalid database name": {"launch": "fixture-launch"},
        "reserved reader prefix": {"reader_role": "pg_reader"},
        "reserved reader name": {"reader_role": "PUBLIC"},
        "invalid reader name": {"reader_role": "fixture.reader"},
        "invalid branch ID": {"branch_id": "fixture01"},
        "unexpected reviewed field": {"extra": "fixture"},
    }.items():
        invalid_pins = reviewed | changes
        check(case, {"reviewed": invalid_pins, "receipt": invalid_pins | {"status": "absent"}})

    check("missing receipt", {"reviewed": reviewed})
    check("absence unconfirmed", record | {"receipt": receipt | {"status": "present"}})
    check("duplicate receipt key", b'{"receipt": null, ' + json.dumps(record).encode()[1:])
    check("oversized record", json.dumps(record).encode().ljust(4097))
    check("boolean OID", {
        "reviewed": reviewed | {"reader_oid": 1}, "receipt": receipt | {"reader_oid": True},
    })
    check("floating-point OID", record | {"receipt": receipt | {"database_oid": 41001.0}})
    check("exact cleanup receipt", record, accept=True)


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


def check_reader_privilege_binding():
    script = Path(__file__).with_name("database_bootstrap_reader_evidence.py")
    retained = b"fixture_catalog.retained: SELECT 1 AS id\n"
    reviewed = b"fixture_catalog.required: SELECT 2 AS id\n" + retained
    previous = b"fixture_catalog.required: SELECT 1 AS id\n" + retained
    inventory = {
        "branch_id": "fixture001", "database_oid": 17001,
        "inventory_sha256": hashlib.sha256(reviewed).hexdigest(),
    }
    review = inventory | {"reader_role": "Fixture-Reader", "reader_oid": 17002}
    privileges = review | {"result": "passed"}
    record = {"review": review, "inventory": inventory, "reader_privileges": privileges}

    def check(case, evidence, verdict="pending"):
        raw = evidence if isinstance(evidence, bytes) else json.dumps(evidence).encode()
        result = subprocess.run(
            [sys.executable, "-IB", str(script)], input=raw, env={},
            capture_output=True, timeout=3, check=False,
        )
        output = f"database-bootstrap-reader-evidence: {verdict}\n".encode()
        expected = (0, output, b"") if verdict == "consistent" else (1, b"", output)
        if (result.returncode, result.stdout, result.stderr) != expected:
            raise RuntimeError(f"reader privilege fixture: {case}")

    # Both observations can follow the same migration run and cover the same reader.
    previous_privileges = privileges | {"inventory_sha256": hashlib.sha256(previous).hexdigest()}
    check("reader privileges cover the earlier definition of the same object",
          record | {"reader_privileges": previous_privileges}, "pending (inventory-mismatch)")
    check("earlier definition reported by a different reader",
          record | {"reader_privileges": previous_privileges | {"reader_oid": 17003}})
    check("earlier definition without a successful privilege check",
          record | {"reader_privileges": previous_privileges | {"result": "failed"}})

    for case, override in {
        "another scoped reader": {"reader_role": "Fixture-Other-Reader", "reader_oid": 17003},
        "same name reissued": {"reader_oid": 17003},
        "same OID different role name": {"reader_role": "Fixture-Other-Reader"},
        "OID encoded as a float": {"reader_oid": 17002.0},
        "unsuccessful privilege result": {"result": "failed"},
        "malformed privilege inventory digest": {"inventory_sha256": "b" * 63},
    }.items():
        check(case, record | {"reader_privileges": privileges | override})
    observed = inventory | {"inventory_sha256": hashlib.sha256(retained).hexdigest()}
    missing_object = record | {"inventory": observed, "reader_privileges": privileges | observed}
    check("complete readback omits a reviewed object", missing_object, "pending (inventory-mismatch)")
    repeated_identity = inventory | {"inventory_sha256": hashlib.sha256(retained * 2).hexdigest()}
    check("readback repeats one identity at the expected row count",
          record | {"inventory": repeated_identity, "reader_privileges": privileges | repeated_identity},
          "pending (inventory-mismatch)")
    check("complete mismatched readback without privilege evidence",
          {"review": review, "inventory": observed}, "pending (inventory-mismatch)")
    check("absent inventory readback", {"review": review, "reader_privileges": privileges})
    check("inventory readback lacks its digest",
          record | {"inventory": {"branch_id": "fixture001", "database_oid": 17001}})
    check("malformed inventory digest", record | {"inventory": inventory | {"inventory_sha256": "b" * 63}})
    check("inventory for another branch", missing_object | {"inventory": observed | {"branch_id": "fixture002"}})
    check("inventory for another database", missing_object | {"inventory": observed | {"database_oid": 17003}})

    # A complete inventory cannot supply scope that the passing access readback lacks.
    for field in ("database_oid", "inventory_sha256"):
        unscoped_privileges = privileges.copy()
        del unscoped_privileges[field]
        check(f"passing privileges without {field}", record | {"reader_privileges": unscoped_privileges})
    check("passing privileges for another database with the same inventory digest",
          record | {"reader_privileges": privileges | {"database_oid": 17004}})

    check("missing privilege evidence", {"review": review, "inventory": inventory})
    check("duplicate privilege object", b'{"reader_privileges": null, ' + json.dumps(record).encode()[1:])
    over = privileges | {"result": "failed", "extra_grants": ["UPDATE ON fixture_catalog.required"]}
    check("readback shows an extra table grant", record | {"reader_privileges": over},
          "pending (extra-grant: UPDATE ON fixture_catalog.required)")
    for grant in (
        "UPDATE (id) ON fixture_catalog.required",
        "UPDATE ON LARGE OBJECT 900001",
        "UPDATE ON fixture_catalog.required WITH GRANT OPTION",
        "SELECT ON fixture_catalog.required WITH GRANT OPTION",
    ):
        check(f"readback names {grant}", record | {"reader_privileges": over | {"extra_grants": [grant]}},
              f"pending (extra-grant: {grant})")
    mixed = ["UPDATE (id) ON fixture_catalog.required", "UPDATE ON LARGE OBJECT 900001"]
    check("readback names mixed table grant forms", record | {"reader_privileges": over | {"extra_grants": mixed}},
          f"pending (extra-grant: {', '.join(mixed)})")
    for case, override in {
        "extra grant beside a passed result": {"result": "passed"},
        "extra grant for another reader": {"reader_oid": 17003},
        "empty extra grant list": {"extra_grants": []},
        "repeated extra grant": {"extra_grants": ["UPDATE ON fixture_catalog.required"] * 2},
        "unnamed extra grant": {"extra_grants": ["UPDATE"]},
        "large object OID exceeds PostgreSQL range": {"extra_grants": ["UPDATE ON LARGE OBJECT 4294967296"]},
        "column list has an empty name": {"extra_grants": ["UPDATE () ON fixture_catalog.required"]},
        "column grant targets a large object": {"extra_grants": ["UPDATE (id) ON LARGE OBJECT 900001"]},
        "column grant uses a table-only privilege": {"extra_grants": ["DELETE (id) ON fixture_catalog.required"]},
        "large object grant uses a table-only privilege": {"extra_grants": ["DELETE ON LARGE OBJECT 900001"]},
    }.items():
        check(case, record | {"reader_privileges": over | override})
    check("oversized record", json.dumps(record).encode().ljust(4097))
    check("privileges match the approved reader and inventory", record, "consistent")


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


def check_inventory_run_binding():
    script = Path(__file__).with_name("database_bootstrap_inventory.py")
    pins = {"branch_id": "fixture001", "database_oid": 40200, "run_id": "8102", "run_attempt": "1"}
    migration = pins | {"completed_at": 1800000000}
    inventory = pins | {"readback_at": 1800000060}
    record = {"migration": migration, "inventory": inventory}

    def check(case, evidence, *, accept=False):
        raw = evidence if isinstance(evidence, bytes) else json.dumps(evidence).encode()
        result = subprocess.run(
            [sys.executable, "-IB", str(script)], input=raw,
            capture_output=True, timeout=3, check=False,
        )
        expected = ((0, b"database-bootstrap-inventory: consistent\n", b"") if accept else
                    (1, b"", b"database-bootstrap-inventory: refused\n"))
        if (result.returncode, result.stdout, result.stderr) != expected:
            raise RuntimeError(f"inventory fixture: {case}")

    check("fresh inventory from previous run", record | {"inventory": inventory | {"run_id": "8101"}})
    check("fresh inventory from current run", record, accept=True)
    for case, override in {
        "different attempt": {"run_attempt": "2"},
        "different database": {"database_oid": 40201},
        "different branch": {"branch_id": "fixture002"},
        "readback before completion": {"readback_at": migration["completed_at"] - 1},
        "readback at completion": {"readback_at": migration["completed_at"]},
        "non-integer readback time": {"readback_at": str(inventory["readback_at"])},
    }.items():
        check(case, record | {"inventory": inventory | override})
    for case, pin in {
        "boolean OID": {"database_oid": True},
        "invalid run ID": {"run_id": "08102"},
        "invalid attempt": {"run_attempt": "0"},
        "invalid branch": {"branch_id": "fixture-01"},
    }.items():
        check(case, {"migration": migration | pin, "inventory": inventory | pin})
    check("missing inventory run ID", record | {
        "inventory": {key: value for key, value in inventory.items() if key != "run_id"},
    })
    check("duplicate migration key", b'{"migration": null, ' + json.dumps(record).encode()[1:])
    check("malformed record", b"{")
    at_limit = json.dumps(record).encode().ljust(4096)
    check("record at size limit", at_limit, accept=True)
    check("record one byte over size limit", at_limit + b" ")


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
    without_capture = snapshot.copy()
    del without_capture["captured_at"]
    check("successful readback missing capture time", record | {"snapshot": without_capture})

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
    late_readback = record["snapshot"] | {"readback_at": review["protected_write_at"] + 1}
    check_snapshot("matching handoff first read back after the protected write",
                   post_setup | baseline_pin | {"snapshot": late_readback}, args=args)
    check_snapshot("retry with matching completed evidence", post_setup | baseline_pin, accept=True, args=args)


def check_snapshot_label_retry():
    record = snapshot_record()
    prior = record["snapshot"]
    setup_completed_at = prior["captured_at"] - 1
    handoff = {
        "review": record["review"] | {"phase": "pre-migration", "setup_completed_at": setup_completed_at},
        "baseline_branch_id": prior["branch_id"],
    }
    args = ("--baseline-handoff",)
    # This synthetic response illustrates what the operator sees before normalizing a readback.
    response = {"message": "Snapshot label fixture-handoff already exists"}
    check_snapshot("retry with only a duplicate-label response", handoff | {"snapshot": response}, args=args)

    # Both readbacks carry the same label outside the checker. The older snapshot
    # completed after setup, but its capture belongs to the pre-setup state.
    other = prior | {"captured_at": setup_completed_at - 1}
    check_snapshot("retry with the older same-label snapshot", handoff | {"snapshot": other}, args=args)
    check_snapshot("retry with the verified prior snapshot", handoff | {"snapshot": prior}, accept=True, args=args)


if __name__ == "__main__":
    try:
        check_baseline_workflow()
        check_aborted_baseline_run_binding()
        check_reader_privilege_binding()
        with tempfile.TemporaryDirectory(prefix="bootstrap-admission-") as temp:
            exercise(Path(temp) / "event.json")
        if Path(temp).exists():
            raise RuntimeError
        check_inventory_run_binding()
        check_snapshot_readback()
        check_snapshot_label_retry()
        check_reader_cleanup_binding()
    except Exception:
        sys.exit("database-bootstrap-admission-fixture: failed")
    print("database-bootstrap-admission-fixture: run binding, snapshot readback, refusals and teardown passed")
