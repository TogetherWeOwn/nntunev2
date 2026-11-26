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


def valid_inventory_digest(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def matches_pins(observed, expected):
    return (
        isinstance(observed, dict) and observed.keys() == expected.keys()
        and all(type(observed[key]) is type(pin) and observed[key] == pin for key, pin in expected.items())
    )


def reader_evidence_verdict(raw):
    if len(raw) > MAX_RECORD_BYTES:
        return "pending"
    record = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    if not isinstance(record, dict) or record.keys() - {"review", "inventory", "reader_privileges"}:
        return "pending"
    review = record.get("review")
    if not isinstance(review, dict) or review.keys() != CATALOG_PINS | READER_PINS:
        return "pending"
    branch, digest, role = review["branch_id"], review["inventory_sha256"], review["reader_role"]
    if not (
        isinstance(branch, str) and re.fullmatch(r"(?:[a-z0-9]{10}|[a-z0-9]{12})", branch)
        and valid_inventory_digest(digest)
        and isinstance(role, str) and re.fullmatch(r"[A-Za-z0-9_-]{1,63}", role)
        and not role.lower().startswith("pg_") and role.lower() != "public"
        and valid_oid(review["database_oid"]) and valid_oid(review["reader_oid"])
    ):
        return "pending"

    catalog = {key: review[key] for key in CATALOG_PINS}
    inventory = record.get("inventory")
    observed_digest = inventory.get("inventory_sha256") if isinstance(inventory, dict) else None
    if not (
        valid_inventory_digest(observed_digest)
        and matches_pins(inventory, catalog | {"inventory_sha256": observed_digest})
    ):
        return "pending"
    if observed_digest != digest:
        return "pending (inventory-mismatch)"

    privileges = record.get("reader_privileges")
    covered_digest = privileges.get("inventory_sha256") if isinstance(privileges, dict) else None
    if not (
        valid_inventory_digest(covered_digest)
        and matches_pins(privileges, review | {"inventory_sha256": covered_digest, "result": "passed"})
    ):
        return "pending"
    return "consistent" if covered_digest == digest else "pending (inventory-mismatch)"


if __name__ == "__main__":
    verdict = "pending"
    try:
        if len(sys.argv) == 1:
            verdict = reader_evidence_verdict(sys.stdin.buffer.read(MAX_RECORD_BYTES + 1))
    except (OSError, ValueError, RecursionError):
        pass
    message = f"database-bootstrap-reader-evidence: {verdict}"
    if verdict != "consistent":
        sys.exit(message)
    print(message)
