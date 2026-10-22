#!/usr/bin/env python3
"""Protected acceptance; Builder must not edit this file.

Runs the real importer AND real validator; only the external psql boundary is
replaced. No network or DB. The stub captures COPY bytes before import.sh removes
its scratch, models UUID/numeric casts, FK reads and transactional cursors, and
rejects unrecognized SQL. It is not a PostgreSQL concurrency/SQL-engine test.

Run via the Rust decoded_typed_fields integration suite (passes the exact built
binary), or MB_VALIDATE_BIN=/path/to/mb-validate python3 <this-file> -v.
Catches decoder-only fixes that let aliases bypass the raw global key preflight,
wrong parent IDs, lost DLQ originals, or normalized hashes/cursor offsets.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import uuid

A = "abcdef00-0000-4000-8000-000000000001"
R = "abcdef00-0000-4000-8000-000000000002"
C = "abcdef00-0000-4000-8000-000000000003"
MISSING = "abcdef00-0000-4000-8000-000000000099"
TS = "2024-02-29 12:34:56+00:00"
TABLES = ("artist", "release", "recording", "track")
HERE = Path(__file__).resolve()
IMPORTER = HERE.parents[1] / "import.sh"


def copy_value(token):
    """Independent COPY text consumer, not the production validator/normalizer."""
    if token == r"\N":
        return None
    out = bytearray()
    raw = token.encode("utf-8")
    i = 0
    named = {ord("b"): 8, ord("f"): 12, ord("n"): 10, ord("r"): 13,
             ord("t"): 9, ord("v"): 11, ord("\\"): 92}
    while i < len(raw):
        b = raw[i]
        i += 1
        if b != 92:
            out.append(b)
            continue
        if i == len(raw):
            raise ValueError("trailing COPY backslash")
        c = raw[i]
        i += 1
        if c in named:
            out.append(named[c])
        elif 48 <= c <= 55:
            digits = bytes([c])
            while i < len(raw) and len(digits) < 3 and 48 <= raw[i] <= 55:
                digits += bytes([raw[i]])
                i += 1
            out.append(int(digits, 8))
        elif c == ord("x"):
            digits = b""
            while i < len(raw) and len(digits) < 2 and chr(raw[i]) in "0123456789abcdefABCDEF":
                digits += bytes([raw[i]])
                i += 1
            if not digits:
                raise ValueError("empty COPY hex escape")
            out.append(int(digits, 16))
        else:
            raise ValueError("unsupported COPY escape at psql boundary")
    return out.decode("utf-8")


def copy_rows(path):
    data = Path(path).read_text(encoding="utf-8")
    return [[copy_value(f) for f in line.split("\t")] for line in data.splitlines()]


def psql_stub():
    state_file = Path(os.environ["COPY_TEST_STATE"])
    state = json.loads(state_file.read_text())
    args = sys.argv[1:]
    sql = args[args.index("-c") + 1] if "-c" in args else sys.stdin.read()
    if "ON_ERROR_STOP=1" not in args:
        raise ValueError("importer lost ON_ERROR_STOP")

    def identity():
        table = re.search(r"table_name = '(\w+)'", sql).group(1)
        digest = re.search(r"dump_id = '([a-f0-9]{64})'", sql).group(1)
        return table, digest, table + ":" + digest

    def save():
        state_file.write_text(json.dumps(state))

    if "CREATE TEMP TABLE want_parent" in sql:
        table = re.search(r"FROM mb_(artist|release|recording) p", sql).group(1)
        path = re.search(r"\\copy want_parent FROM '([^']+)'", sql).group(1)
        parents = copy_rows(path)
        bad = [line for key, line in parents if str(uuid.UUID(key)) not in state["rows"][table]]
        state["events"].append({"kind": "fk", "table": table, "parents": parents})
        save()
        print(" ".join(bad))
    elif "RETURNING lines_done" in sql:
        table, digest, cursor = identity()
        if "BEGIN;" not in sql or "COMMIT;" not in sql:
            raise ValueError("chunk lost transaction")
        stage_path = re.search(r"\\copy stage .*? FROM '([^']+)'", sql).group(1)
        poison_path = re.search(r"\\copy poison FROM '([^']+)'", sql).group(1)
        stage = copy_rows(stage_path)
        poison = copy_rows(poison_path)
        wire = Path(stage_path).read_text().splitlines()
        offset = int(re.search(r"AND lines_done = (\d+)", sql).group(1))
        end = int(re.search(r"SET lines_done = (\d+)", sql).group(1))
        if state["cursors"].get(cursor) != offset:
            print("")
            return
        converted = []
        for fields in stage:
            fields[0] = str(uuid.UUID(fields[0]))
            if table != "artist" and fields[1] is not None:
                fields[1] = str(uuid.UUID(fields[1]))
            numeric = {"artist": [], "release": [4], "recording": [3, 4], "track": [2, 3]}[table]
            for index in numeric:
                if fields[index] is not None:
                    fields[index] = str(int(fields[index]))
            key = "/".join([fields[0], fields[2], fields[3]]) if table == "track" else fields[0]
            converted.append((key, fields))
        # Retain every emitted stage row in events even when an upsert would
        # collapse aliases: the importer, not our fake DB, must refuse them.
        for key, fields in converted:
            state["rows"][table][key] = fields
        for reason, line, raw in poison:
            state["dlq"].setdefault(cursor + ":" + line, [table, digest, line, reason, raw])
        state["cursors"][cursor] = end
        state["events"].append({"kind": "commit", "table": table, "digest": digest,
                                "offset": offset, "end": end, "stage": stage,
                                "wire": wire, "poison": poison})
        save()
        print(end)
    elif "SELECT lines_done FROM mb_import_cursor" in sql:
        print(state["cursors"].get(identity()[2], 0))
    elif "SELECT count(*) FROM mb_import_dlq" in sql:
        table, digest, _ = identity()
        print(sum(x[0] == table and x[1] == digest for x in state["dlq"].values()))
    elif "INSERT INTO mb_import_cursor" in sql:
        table, digest = re.search(r"VALUES \('(\w+)', '([a-f0-9]{64})', 0\)", sql).groups()
        state["cursors"].setdefault(table + ":" + digest, 0)
        save()
    else:
        raise ValueError("unrecognized SQL at offline psql boundary")


def literals():
    return {
        "artist": [[A, "Name", "Sort", "Person", "US", TS]],
        "release": [[R, A, "Title", "Album", "2024", r"\N", TS]],
        "recording": [[C, R, "Title", "1", "1234", TS]],
        "track": [[R, C, "1", "2", TS]],
    }


def encoded_uuid(key, spelling=r"\x61"):
    return spelling + key[1:]


class ImportDecodedFields(unittest.TestCase):
    def setUp(self):
        validator = os.environ.get("MB_VALIDATE_BIN")
        if not validator or not os.access(validator, os.X_OK):
            raise RuntimeError("MB_VALIDATE_BIN must name the real compiled mb-validate; no fake validator")
        root = os.environ.get("PAPERCLIP_RUN_SCRATCH_DIR") or os.environ.get("PAPERCLIP_SCRATCH_DIR")
        self.temp = tempfile.TemporaryDirectory(prefix="copy-typed-import-", dir=root)
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.dump = self.root / "dump"
        self.dump.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Execute this test's psql-only mode, never a real psql binary.
        psql = self.bin / "psql"
        psql.write_text("#!/usr/bin/env python3\nimport runpy\nrunpy.run_path(" + repr(str(HERE)) + ", run_name='__psql_stub__')\n")
        psql.chmod(0o755)
        self.state_path = self.root / "state.json"
        self.reset()
        self.validator = str(Path(validator).resolve())

    def reset(self):
        self.state_path.write_text(json.dumps({"rows": {t: {} for t in TABLES},
                                              "cursors": {}, "dlq": {}, "events": []}))

    def state(self):
        return json.loads(self.state_path.read_text())

    def write_dump(self, rows):
        for table, data in rows.items():
            (self.dump / f"mbdump-{table}.tsv").write_text("".join("\t".join(r) + "\n" for r in data))

    def digest(self, table):
        return hashlib.sha256((self.dump / f"mbdump-{table}.tsv").read_bytes()).hexdigest()

    def run_import(self, chunk=1):
        env = {"PATH": str(self.bin) + os.pathsep + os.environ["PATH"],
               "HOME": str(self.root), "TMPDIR": str(self.root),
               "DATABASE_URL": "postgresql://fixture@database.invalid/offline",
               "MB_VALIDATE_BIN": self.validator, "MB_IMPORT_CHUNK_LINES": str(chunk),
               "COPY_TEST_STATE": str(self.state_path), "LC_ALL": "C.UTF-8"}
        return subprocess.run(["bash", str(IMPORTER), str(self.dump)], env=env,
                              capture_output=True, text=True, timeout=30)

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("import: done", result.stdout)

    def test_literal_normal_flow_and_completed_resume_keep_raw_hashes(self):
        rows = literals()
        rows["track"].append([R, C, "1", "3", TS])  # same release != duplicate position
        self.write_dump(rows)
        self.assert_success(self.run_import())
        first = self.state()
        self.assertEqual(len(first["rows"]["track"]), 2)
        self.assertEqual(first["dlq"], {})
        for table in TABLES:
            self.assertEqual(first["cursors"][table + ":" + self.digest(table)], len(rows[table]))
        self.assert_success(self.run_import())
        self.assertEqual(self.state(), first, "completed resume must not COPY/rewrite cursors or DLQ")

    def test_escaped_typed_values_resolve_parents_and_emit_copy_safe_rows(self):
        rows = literals()
        rows["artist"][0] = [encoded_uuid(A), r"literal\\N\tname\nend", "Sort", r"\\N", r"\x55S", r"\062024-02-29\04012:34:56+00:00"]
        rows["release"][0] = [encoded_uuid(R, r"\141"), encoded_uuid(A, r"\141"), "Title", "Album", r"\x32024", r"\N", TS]
        rows["recording"][0] = [encoded_uuid(C), encoded_uuid(R), "Title", r"\061", r"\x31234", TS]
        rows["track"][0] = [encoded_uuid(R), encoded_uuid(C, r"\141"), r"\061", r"\x32", TS]
        self.write_dump(rows)
        self.assert_success(self.run_import())
        state = self.state()
        self.assertEqual(state["dlq"], {}, "escaped existing parents must not become orphans")
        self.assertEqual(state["rows"]["artist"][A][1:5], ["literal\\N\tname\nend", "Sort", "\\N", "US"])
        self.assertEqual(state["rows"]["release"][R][1], A)
        self.assertEqual(state["rows"]["recording"][C][1:5], [R, "Title", "1", "1234"])
        self.assertEqual(state["rows"]["track"][R + "/1/2"][:4], [R, C, "1", "2"])
        for table in TABLES:
            self.assertEqual(state["cursors"], {t + ":" + self.digest(t): 1 for t in TABLES})
            commits = [e for e in state["events"] if e["kind"] == "commit" and e["table"] == table]
            self.assertEqual(len(commits), 1)
            self.assertEqual(len(commits[0]["wire"]), 1)
            self.assertEqual(len(commits[0]["wire"][0].split("\t")), len(rows[table][0]) + 1)
            self.assertEqual(commits[0]["stage"][0][-1], "1")

    def test_entity_alias_duplicates_are_refused_globally_before_any_target_chunk(self):
        for table in ("artist", "release", "recording"):
            for spelling in (r"\141", r"\x61"):
                for chunk in (1, 2):
                    with self.subTest(table=table, spelling=spelling, chunk=chunk):
                        self.reset()
                        rows = literals()
                        duplicate = rows[table][0].copy()
                        duplicate[0] = encoded_uuid(duplicate[0], spelling)
                        rows[table].append(duplicate)
                        self.write_dump(rows)
                        result = self.run_import(chunk)
                        self.assertNotEqual(result.returncode, 0, "duplicate aliases must refuse")
                        self.assertIn("duplicate key", result.stderr, result.stdout + result.stderr)
                        commits = [e for e in self.state()["events"] if e["kind"] == "commit" and e["table"] == table]
                        self.assertEqual(commits, [], "global duplicate preflight must run before chunk 1 commits")

    def test_track_alias_duplicates_cannot_cross_a_chunk_or_resume_boundary(self):
        for field, alias in ((0, encoded_uuid(R)), (2, r"\061"), (3, r"\x32"), (2, "+001")):
            for completed in (0, 1):
                with self.subTest(field=field, alias=alias, completed=completed):
                    self.reset()
                    rows = literals()
                    duplicate = rows["track"][0].copy()
                    duplicate[field] = alias
                    rows["track"].append(duplicate)
                    self.write_dump(rows)
                    if completed:
                        state = self.state()
                        state["cursors"]["track:" + self.digest("track")] = 1
                        state["rows"]["track"][R + "/1/2"] = [R, C, "1", "2", TS, "1"]
                        self.state_path.write_text(json.dumps(state))
                    result = self.run_import(chunk=1)
                    self.assertNotEqual(result.returncode, 0, "track key alias must refuse")
                    self.assertIn("duplicate key", result.stderr, result.stdout + result.stderr)
                    self.assertEqual(self.state()["cursors"]["track:" + self.digest("track")], completed)
                    self.assertFalse(any(e["kind"] == "commit" and e["table"] == "track" for e in self.state()["events"]))

    def test_literal_duplicates_still_refuse_but_distinct_keys_do_not(self):
        for table in TABLES:
            with self.subTest(table=table):
                self.reset()
                rows = literals()
                rows[table].append(rows[table][0].copy())
                self.write_dump(rows)
                result = self.run_import()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("duplicate key", result.stderr)
                self.assertFalse(any(e["kind"] == "commit" and e["table"] == table for e in self.state()["events"]))

    def test_poison_and_fk_orphan_keep_original_tokens_line_numbers_and_resume_accounting(self):
        rows = literals()
        poison = [MISSING, r"original\tname\\N", "Sort", "Person", r"\t", TS]
        orphan = [MISSING, encoded_uuid(MISSING), r"orphan\nname", "Album", r"\062024", r"\N", TS]
        rows["artist"].append(poison)
        rows["release"].append(orphan)
        self.write_dump(rows)
        self.assert_success(self.run_import())
        state = self.state()
        dlq = {x[0]: x for x in state["dlq"].values()}
        self.assertEqual(set(dlq), {"artist", "release"})
        self.assertEqual(dlq["artist"], ["artist", self.digest("artist"), "2", "bad_country", "\t".join(poison)])
        self.assertEqual(dlq["release"], ["release", self.digest("release"), "2", "fk_missing_artist", "\t".join(orphan)])
        for table, count in (("artist", 2), ("release", 2), ("recording", 1), ("track", 1)):
            self.assertEqual(state["cursors"][table + ":" + self.digest(table)], count)
            commits = [e for e in state["events"] if e["kind"] == "commit" and e["table"] == table]
            self.assertEqual([(e["offset"], e["end"]) for e in commits], [(i, i + 1) for i in range(count)])
            self.assertEqual(sum(len(e["stage"]) + len(e["poison"]) for e in commits), count)
        self.assert_success(self.run_import())
        self.assertEqual(self.state(), state, "resume must not duplicate DLQ rows or re-run committed chunks")


if __name__ == "__psql_stub__":
    psql_stub()
elif __name__ == "__main__":
    unittest.main()
