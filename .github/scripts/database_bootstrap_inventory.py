"""Check the binding of a normalized schema inventory to a completed migration."""

import json
import re
import sys

MAX_RECORD_BYTES = 4096
MAX_TIMESTAMP = 253402300799  # End of year 9999 in Unix seconds.
BRANCH_ID = re.compile(r"(?:[a-z0-9]{10}|[a-z0-9]{12})")
RUN_NUMBER = re.compile(r"[1-9][0-9]{0,19}")
PINS = ("branch_id", "database_oid", "run_id", "run_attempt")


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def consistent_inventory(raw):
    if len(raw) > MAX_RECORD_BYTES:
        return False
    record = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    if not isinstance(record, dict) or record.keys() != {"migration", "inventory"}:
        return False
    migration, inventory = record["migration"], record["inventory"]
    pin_fields = set(PINS)
    if not (
        isinstance(migration, dict) and migration.keys() == pin_fields | {"completed_at"}
        and isinstance(inventory, dict) and inventory.keys() == pin_fields | {"readback_at"}
    ):
        return False

    for evidence in (migration, inventory):
        branch, oid, run, attempt = (evidence[field] for field in PINS)
        if not (
            isinstance(branch, str) and BRANCH_ID.fullmatch(branch)
            and type(oid) is int and 0 < oid <= 0xFFFFFFFF
            and isinstance(run, str) and RUN_NUMBER.fullmatch(run)
            and isinstance(attempt, str) and RUN_NUMBER.fullmatch(attempt)
        ):
            return False

    completed, readback = migration["completed_at"], inventory["readback_at"]
    return (
        all(inventory[field] == migration[field] for field in PINS)
        and type(completed) is int and type(readback) is int
        and 0 < completed < readback <= MAX_TIMESTAMP
    )


if __name__ == "__main__":
    try:
        consistent = len(sys.argv) == 1 and consistent_inventory(sys.stdin.buffer.read(MAX_RECORD_BYTES + 1))
    except (OSError, ValueError, RecursionError):
        consistent = False
    if not consistent:
        sys.exit("database-bootstrap-inventory: refused")
    print("database-bootstrap-inventory: consistent")
