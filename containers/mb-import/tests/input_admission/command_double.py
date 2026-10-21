#!/usr/bin/env python3
"""Strict offline process doubles, not a PostgreSQL implementation.

Capture SQL and COPY bytes before import.sh deletes its scratch. Maintain only
cursor/DLQ/state replies needed to exercise the real shell's control flow.
Unknown SQL fails loudly; no code path connects to a database. The validator
only recognizes synthetic bad_uuid fixture rows; native validation is covered
by the existing validator/integration suites, not this admission suite.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys
import uuid


def append(path, value):
    with path.open("a") as f:
        f.write(json.dumps(value) + "\n")


def switch_publication(root, table, point):
    config = json.loads((root / "config.json").read_text())
    marker = root / "switched"
    if table == "artist" and config.get("switch") == point and not marker.exists():
        link = root / "curated"
        pending = root / "curated.next"
        pending.symlink_to("publication B")
        os.replace(pending, link)
        marker.write_text(point)


def psql(root):
    args = sys.argv[1:]
    commands = [args[i + 1] for i, a in enumerate(args[:-1]) if a in ("-c", "--command")]
    if not commands:
        commands = [sys.stdin.read()]
    state_path = root / "state.json"
    state = json.loads(state_path.read_text())
    for sql in commands:
        copies = {}
        for name, path in re.findall(r"\\copy\s+(\w+).*?FROM\s+'([^']+)'", sql, re.I):
            copies[name] = Path(path).read_text()
        # Record ANY attempted SQL mutation, including temp DDL and transaction
        # bodies, before simulating rejection. A rolled-back write is still an
        # admission violation. Reads are allowed before preflight completes.
        mutating = bool(re.search(r"\b(INSERT|UPDATE|DELETE|CREATE|ALTER|TRUNCATE|DROP|COPY)\b", sql, re.I))
        append(root / "psql.jsonl", {"sql": sql, "copies": copies, "mutation": mutating})
        if "ON_ERROR_STOP=1" not in args:
            raise RuntimeError("importer dropped ON_ERROR_STOP")
        table_match = re.search(r"table_name\s*=\s*'([a-z]+)'", sql)
        dump_match = re.search(r"dump_id\s*=\s*'([0-9a-f]{64})'", sql)
        table = table_match[1] if table_match else None
        key = f"{table}:{dump_match[1]}" if dump_match else None
        if re.search(r"INSERT INTO mb_import_cursor", sql):
            match = re.search(r"VALUES\s*\('([a-z]+)',\s*'([0-9a-f]{64})',\s*0\)", sql)
            if not match:
                raise RuntimeError("unsupported cursor INSERT")
            state["cursors"].setdefault(f"{match[1]}:{match[2]}", 0)
            switch_publication(root, match[1], "artist_cursor")
        elif "SELECT lines_done FROM mb_import_cursor" in sql:
            print(state["cursors"].get(key, ""))
        elif "SELECT count(*) FROM mb_import_dlq" in sql:
            print(sum(1 for row in state["dlq"] if row[0] == key))
            switch_publication(root, table, "artist_complete")
        elif "CREATE TEMP TABLE want_parent" in sql:
            config = json.loads((root / "config.json").read_text())
            # One known synthetic orphan: return it only while still queried.
            bad = config.get("fk_bad")
            lines = copies["want_parent"].splitlines()
            print(" ".join(line.split("\t")[1] for line in lines
                           if line.split("\t")[0] == bad))
        elif "RETURNING lines_done" in sql:
            end = int(re.search(r"SET lines_done\s*=\s*(\d+)", sql)[1])
            offset = int(re.search(r"AND lines_done\s*=\s*(\d+)", sql)[1])
            if state["cursors"].get(key) != offset:
                raise RuntimeError("cursor compare-and-set mismatch")
            if not all(word in sql for word in ("BEGIN;", "COMMIT;", "INSERT INTO mb_", "INSERT INTO mb_import_dlq")):
                raise RuntimeError("unsupported chunk transaction")
            state["cursors"][key] = end
            for line in copies["poison"].splitlines():
                reason, line_no, raw = line.split("\t", 2)
                row = [key, int(line_no), reason, raw]
                if row not in state["dlq"]:
                    state["dlq"].append(row)
            append(root / "chunks.jsonl", {"table": table, "dump_id": dump_match[1],
                                          "end": end, "offset": offset,
                                          "valid": copies["stage"], "poison": copies["poison"], "sql": sql})
            print(end)
        elif "SELECT last_packet FROM mb_import_state" in sql:
            print(state["last_packet"])
        elif "UPDATE mb_import_state SET last_packet" in sql:
            match = re.search(r"SET last_packet\s*=\s*([0-9]+)", sql)
            if not match:
                raise RuntimeError("unsupported state UPDATE")
            sequence = int(match[1])
            if sequence > 2147483647:
                print("psql double: integer out of range", file=sys.stderr)
                return 1
            if not re.search(r"WHERE id\s*=\s*1 AND last_packet\s*<\s*[0-9]+", sql):
                raise RuntimeError("state seed lost forward-only predicate")
            state["last_packet"] = max(state["last_packet"], sequence)
        else:
            raise RuntimeError("unsupported SQL: " + sql)
        state_path.write_text(json.dumps(state))
    return 0


def validate(root):
    parser = argparse.ArgumentParser()
    parser.add_argument("--table", required=True)
    parser.add_argument("--in", dest="source", required=True)
    parser.add_argument("--valid-out", required=True)
    parser.add_argument("--poison-out", required=True)
    parser.add_argument("--line-offset", type=int, required=True)
    args = parser.parse_args()
    config = json.loads((root / "config.json").read_text())
    raw = Path(args.source).read_text()
    append(root / "validator.jsonl", {"table": args.table, "offset": args.line_offset, "raw": raw})
    if config.get("fail_track_tail") and args.table == "track" and args.line_offset > 0:
        print("injected validator failure on track tail", file=sys.stderr)
        return 2
    valid, poison = [], []
    for i, line in enumerate(raw.splitlines(), args.line_offset + 1):
        try:
            uuid.UUID(line.split("\t")[0])
        except ValueError:
            escaped = line.replace("\\", "\\\\").replace("\t", "\\t")
            poison.append(f"bad_uuid\t{i}\t{escaped}\n")
        else:
            valid.append(f"{line}\t{i}\n")
    Path(args.valid_out).write_text("".join(valid))
    Path(args.poison_out).write_text("".join(poison))
    print(f"valid={len(valid)} poison={len(poison)}")
    return 0


if __name__ == "__main__":
    capture = Path(os.environ["INPUT_ADMISSION_CAPTURE"])
    try:
        sys.exit(psql(capture) if Path(sys.argv[0]).name == "psql" else validate(capture))
    except Exception as exc:
        append(capture / "harness-errors.jsonl", str(exc))
        raise
