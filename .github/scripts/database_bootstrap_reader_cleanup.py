"""Compare saved launch-reader cleanup evidence with the reviewed activation pins."""

import json
import re
import sys

from database_bootstrap_snapshot import BRANCH_ID, MAX_RECORD_BYTES, unique_fields

OIDS = {"database_oid", "reader_oid"}
PINS = {"branch_id", "launch", "reader_role"} | OIDS


def valid_pins(pins):
    return (
        isinstance(pins, dict) and pins.keys() == PINS
        and all(type(pins[key]) is int and 0 < pins[key] <= 4294967295 for key in OIDS)
        and all(isinstance(pins[key], str) for key in PINS - OIDS)
        and BRANCH_ID.fullmatch(pins["branch_id"])
        and re.fullmatch(r"[a-z_][a-z0-9_]{0,62}", pins["launch"])
        and pins["launch"] not in ("postgres", "template0", "template1")
        and re.fullmatch(r"[A-Za-z0-9_-]{1,63}", pins["reader_role"])
        and not pins["reader_role"].lower().startswith("pg_")
        and pins["reader_role"].lower() != "public"
    )


def consistent_readback(raw):
    if len(raw) > MAX_RECORD_BYTES:
        return False
    try:
        record = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields)
    except (ValueError, RecursionError):
        return False
    if not isinstance(record, dict) or record.keys() != {"reviewed", "receipt"}:
        return False
    reviewed, receipt = record["reviewed"], record["receipt"]
    return (
        valid_pins(reviewed)
        and isinstance(receipt, dict)
        and receipt == (reviewed | {"status": "absent"})
        and all(type(receipt[key]) is int for key in OIDS)
    )


if __name__ == "__main__":
    try:
        consistent = len(sys.argv) == 1 and consistent_readback(
            sys.stdin.buffer.read(MAX_RECORD_BYTES + 1),
        )
    except OSError:
        consistent = False
    if not consistent:
        sys.exit("database-bootstrap-reader-cleanup: pending")
    print("database-bootstrap-reader-cleanup: consistent")
