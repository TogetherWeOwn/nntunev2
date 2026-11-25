"""Check the consistency of a normalized native snapshot readback offline."""

import json
import re
import sys

MAX_RECORD_BYTES = 4096
MAX_TIMESTAMP = 253402300799  # End of year 9999 in Unix seconds.
BRANCH_ID = re.compile(r"(?:[a-z0-9]{10}|[a-z0-9]{12})")
REVIEW_FIELDS = {"phase", "branch_id", "protected_write_at", "window_end_at"}
SNAPSHOT_FIELDS = {
    "kind", "branch_id", "restore_scope", "status",
    "captured_at", "completed_at", "readback_at", "expires_at",
}


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def consistent_readback(raw, *, baseline_handoff=False):
    if len(raw) > MAX_RECORD_BYTES:
        return False
    record = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    fields = {"review", "snapshot", "baseline_branch_id"} if baseline_handoff else {"review", "snapshot"}
    if not isinstance(record, dict) or record.keys() != fields:
        return False
    review, snapshot = record["review"], record["snapshot"]
    if not isinstance(review, dict) or not isinstance(snapshot, dict):
        return False
    phase = review.get("phase")
    if phase not in ("pre-bootstrap", "pre-migration"):
        return False
    post_setup = phase == "pre-migration"
    fields = REVIEW_FIELDS | {"setup_completed_at"} if post_setup else REVIEW_FIELDS
    if review.keys() != fields or snapshot.keys() != SNAPSHOT_FIELDS:
        return False

    branch_id = review["branch_id"]
    if baseline_handoff and (not post_setup or record["baseline_branch_id"] != branch_id):
        return False
    if not (
        isinstance(branch_id, str) and BRANCH_ID.fullmatch(branch_id)
        and snapshot["branch_id"] == branch_id
        and snapshot["kind"] == "ordinary-native"
        and snapshot["restore_scope"] == "whole-branch"
        and snapshot["status"] == "succeeded"
    ):
        return False

    captured, completed, readback, expires = (
        snapshot["captured_at"], snapshot["completed_at"], snapshot["readback_at"], snapshot["expires_at"]
    )
    write, window_end = review["protected_write_at"], review["window_end_at"]
    times = (captured, completed, readback, write, window_end, expires)
    if post_setup:
        times += (review["setup_completed_at"],)
    if any(type(value) is not int or not 0 < value <= MAX_TIMESTAMP for value in times):
        return False
    return (
        captured <= completed <= readback < write <= window_end < expires
        and (not post_setup or review["setup_completed_at"] < captured)
    )


if __name__ == "__main__":
    try:
        args = sys.argv[1:]
        consistent = args in ([], ["--baseline-handoff"]) and consistent_readback(
            sys.stdin.buffer.read(MAX_RECORD_BYTES + 1), baseline_handoff=bool(args),
        )
    except (OSError, ValueError, RecursionError):
        consistent = False
    if not consistent:
        sys.exit("database-bootstrap-snapshot: refused")
    print("database-bootstrap-snapshot: consistent")
