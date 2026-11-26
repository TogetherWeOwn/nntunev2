"""Check saved first-launch inventory and reader privilege evidence offline."""

import json
import re
import sys

MAX_RECORD_BYTES = 4096
CATALOG_PINS = {"branch_id", "database_oid", "inventory_sha256"}
READER_PINS = {"reader_role", "reader_oid"}


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def valid_oid(value):
    return type(value) is int and 0 < value <= 0xFFFFFFFF


def matches_pins(observed, expected):
    return (
        isinstance(observed, dict) and observed.keys() == expected.keys()
        and all(type(observed[key]) is type(pin) and observed[key] == pin for key, pin in expected.items())
    )


def consistent_reader_evidence(raw):
    if len(raw) > MAX_RECORD_BYTES:
        return False
    record = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    if not isinstance(record, dict) or record.keys() != {"review", "inventory", "reader_privileges"}:
        return False
    review = record["review"]
    if not isinstance(review, dict) or review.keys() != CATALOG_PINS | READER_PINS:
        return False
    branch, digest, role = review["branch_id"], review["inventory_sha256"], review["reader_role"]
    if not (
        isinstance(branch, str) and re.fullmatch(r"(?:[a-z0-9]{10}|[a-z0-9]{12})", branch)
        and isinstance(digest, str) and re.fullmatch(r"[0-9a-f]{64}", digest)
        and isinstance(role, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,63}", role)
        and not role.lower().startswith("pg_") and role.lower() != "public"
        and valid_oid(review["database_oid"]) and valid_oid(review["reader_oid"])
    ):
        return False

    catalog = {key: review[key] for key in CATALOG_PINS}
    return (
        matches_pins(record["inventory"], catalog)
        and matches_pins(record["reader_privileges"], review | {"result": "passed"})
    )


if __name__ == "__main__":
    try:
        consistent = len(sys.argv) == 1 and consistent_reader_evidence(sys.stdin.buffer.read(MAX_RECORD_BYTES + 1))
    except (OSError, ValueError, RecursionError):
        consistent = False
    if not consistent:
        sys.exit("database-bootstrap-reader-evidence: pending")
    print("database-bootstrap-reader-evidence: consistent")
