#!/usr/bin/env python3
"""Run the real importer at its process boundary, with no live services.

Breaks caught: following a replaced publication link, checking later files or
DUMP_SEQ after a mutation, accepting overflowing sequence arithmetic, ignoring
saved cursors, dropping poison/FK routing, or pruning an unfinished track tail.
Database execution semantics remain the responsibility of test-import.sh.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid

HERE = Path(__file__).resolve().parent
IMPORTER = HERE.parents[1] / "import.sh"
TABLES = ("artist", "release", "recording", "track")


def read_events(path):
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []


def make_dump(path, label="A", count=2):
    path.mkdir()
    stamp = "2025-01-01T00:00:00Z" if label == "A" else "2025-02-01T00:00:00Z"
    def uid(n):
        return str(uuid.UUID(int=n))
    for table in TABLES:
        rows = []
        for i in range(1, count + 1):
            fields = {
                "artist": [uid(i), f"{label} artist {i}", f"{label} sort {i}", "Group", "US", stamp],
                "release": [uid(100 + i), uid(i), f"{label} release {i}", "Album", "2025", r"\N", stamp],
                "recording": [uid(200 + i), uid(100 + i), f"{label} recording {i}", "1", "180000", stamp],
                "track": [uid(100 + i), uid(200 + i), "1", "1", stamp],
            }[table]
            rows.append("\t".join(fields) + "\n")
        (path / f"mbdump-{table}.tsv").write_text("".join(rows))


class InputAdmission(unittest.TestCase):
    def setUp(self):
        scratch = os.environ.get("PAPERCLIP_RUN_SCRATCH_DIR") or os.environ.get("PAPERCLIP_SCRATCH_DIR")
        self.root = Path(tempfile.mkdtemp(prefix="input-admission-", dir=scratch))
        if os.environ.get("INPUT_ADMISSION_KEEP") == "1":
            print(f"capture {self.id().split('.')[-1]}: {self.root}", flush=True)
        else:
            self.addCleanup(shutil.rmtree, self.root)
        for directory in ("bin", "tmp", "home"):
            (self.root / directory).mkdir()
        for name in ("psql", "mb-validate"):
            shutil.copyfile(HERE / "command_double.py", self.root / "bin" / name)
            (self.root / "bin" / name).chmod(0o755)
        self.a = self.root / "publication A"
        self.b = self.root / "publication B"
        make_dump(self.a)
        make_dump(self.b, "B", 3)
        self.link = self.root / "curated"
        self.link.symlink_to(self.a.name)
        self.state_file = self.root / "state.json"
        self.state_file.write_text(json.dumps({"cursors": {}, "dlq": [], "last_packet": 41,
                                               "last_applied_at": "2024-01-01", "lag_seconds": 17}))
        self.config = {}
        self.run_number = 0

    def state(self):
        return json.loads(self.state_file.read_text())

    def events(self, name="psql"):
        return read_events(self.root / f"{name}.jsonl")

    def run_import(self, sequence=None, directory="curated"):
        self.run_number += 1
        (self.root / "config.json").write_text(json.dumps(self.config))
        env = {
            "PATH": str(self.root / "bin") + os.pathsep + os.defpath + os.pathsep + str(Path(sys.executable).parent),
            "HOME": str(self.root / "home"), "TMPDIR": str(self.root / "tmp"),
            "INPUT_ADMISSION_CAPTURE": str(self.root), "MB_VALIDATE_BIN": str(self.root / "bin" / "mb-validate"),
            "MB_IMPORT_CHUNK_LINES": "2", "LC_ALL": "C",
            # Synthetic .invalid endpoint: only the local psql double can run.
            "DATABASE_URL": "postgresql://fixture@database.invalid/fixture",
        }
        if sequence is not None:
            env["DUMP_SEQ"] = sequence
        result = subprocess.run(["bash", str(IMPORTER), str(directory)], cwd=self.root, env=env,
                                stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)
        for name, text in (("stdout", result.stdout), ("stderr", result.stderr)):
            (self.root / f"run-{self.run_number}.{name}").write_text(text)
        self.assertEqual(self.events("harness-errors"), [], "command-double protocol error, not a product failure")
        self.assertNotIn("not on PATH", result.stderr, "missing test tool, not a product failure")
        self.assertNotIn("not executable", result.stderr, "missing validator double, not a product failure")
        return result

    def assert_success(self, result):
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("import: done", result.stdout)

    def assert_refused_without_writes(self, result, before):
        mutations = [event for event in self.events() if event["mutation"]]
        # Check attempts first: a later psql refusal/rollback is not admission.
        self.assertEqual(len(mutations), 0,
                         f"preflight must precede every mutation; observed {len(mutations)} mutation attempts")
        self.assertNotEqual(result.returncode, 0, "invalid input was reported as a successful import")
        self.assertEqual(self.state(), before, "cursor/DLQ/replication state changed on refused admission")
        self.assertEqual(self.events("chunks"), [], "entity chunk was submitted on refused admission")

    def assert_loaded_publication(self, chunks, publication):
        for table in TABLES:
            expected = (publication / f"mbdump-{table}.tsv").read_text()
            loaded = [chunk for chunk in chunks if chunk["table"] == table]
            self.assertTrue(loaded, f"no {table} payload submitted")
            raw = "".join(line.rsplit("\t", 1)[0] + "\n"
                          for chunk in loaded for line in chunk["valid"].splitlines())
            self.assertEqual(raw, expected, f"{table} COPY bytes mixed publications")
            digest = hashlib.sha256(expected.encode()).hexdigest()
            self.assertEqual({chunk["dump_id"] for chunk in loaded}, {digest},
                             f"{table} cursor hash and COPY bytes must name the same publication")
            self.assertEqual(loaded[-1]["end"], len(expected.splitlines()),
                             f"{table} count must come from the pinned publication")

    def publication_switch(self, point):
        self.config["switch"] = point
        result = self.run_import()
        self.assert_success(result)
        self.assertEqual((self.root / "switched").read_text(), point, "synchronization hook never ran")
        self.assertEqual(self.link.resolve(), self.b)
        first = self.events("chunks")
        # Subsequent run must resolve afresh, rather than cache A globally.
        self.assert_success(self.run_import())
        second = self.events("chunks")[len(first):]
        self.assert_loaded_publication(first, self.a)
        self.assert_loaded_publication(second, self.b)

    def test_symlink_replaced_after_artist_phase_still_imports_only_A_then_B(self):
        self.publication_switch("artist_complete")

    def test_symlink_replaced_between_artist_hash_and_chunk_read_still_imports_A(self):
        self.publication_switch("artist_cursor")

    def test_ordinary_directory_with_spaces_and_unset_sequence(self):
        before = self.state()
        self.assert_success(self.run_import(directory=self.a))
        self.assert_loaded_publication(self.events("chunks"), self.a)
        after = self.state()
        for key in ("last_packet", "last_applied_at", "lag_seconds"):
            self.assertEqual(after[key], before[key])
        self.assertFalse(any("UPDATE mb_import_state" in event["sql"] for event in self.events()))

    def test_zero_sequence_on_migration_seed_is_a_noop(self):
        state = self.state()
        state["last_packet"] = 0
        self.state_file.write_text(json.dumps(state))
        self.assert_success(self.run_import(sequence="0"))
        self.assertEqual(self.state()["last_packet"], 0)
        self.assertFalse(any("UPDATE mb_import_state" in event["sql"] for event in self.events()))

    def test_all_poison_refuses_on_first_import_and_completed_rerun(self):
        artist = self.a / "mbdump-artist.tsv"
        artist.write_text("not-a-uuid\tBad\tBad\tGroup\tUS\t2025-01-01T00:00:00Z\n")
        first = self.run_import(sequence="100")
        self.assertNotEqual(first.returncode, 0)
        self.assertEqual(len(self.state()["dlq"]), 1)
        self.assertEqual(self.state()["last_packet"], 41)
        chunks, before = self.events("chunks"), self.state()
        self.assertEqual([chunk["table"] for chunk in chunks], ["artist"])
        second = self.run_import(sequence="100")
        self.assertNotEqual(second.returncode, 0)
        self.assertEqual(self.events("chunks"), chunks)
        self.assertEqual(self.state(), before)
        self.assertFalse(any("UPDATE mb_import_state" in event["sql"] for event in self.events()))

    def test_same_dump_rerun_skips_every_completed_chunk(self):
        self.assert_success(self.run_import())
        before, chunks = self.state(), self.events("chunks")
        self.assert_success(self.run_import())
        self.assertEqual(self.state(), before)
        self.assertEqual(self.events("chunks"), chunks, "completed dump was submitted a second time")

    def test_newer_dump_gets_independent_hashes_and_forward_only_upsert(self):
        self.assert_success(self.run_import())
        first = self.events("chunks")
        self.assert_success(self.run_import(directory=self.b))
        newer = self.events("chunks")[len(first):]
        self.assert_loaded_publication(newer, self.b)
        self.assertEqual(len(self.state()["cursors"]), 8)
        # Contract emitted to PostgreSQL, not a claim of emulated DB semantics.
        for chunk in newer:
            self.assertIn(f"WHERE mb_{chunk['table']}.updated_at < EXCLUDED.updated_at", chunk["sql"])

    def test_poison_and_fk_orphans_keep_original_line_numbers_and_raw_bytes(self):
        artist = self.a / "mbdump-artist.tsv"
        bad = "not-a-uuid\tBad\tBad\tGroup\tUS\t2025-01-01T00:00:00Z\n"
        artist.write_text(artist.read_text() + bad)
        orphan = "00000000-0000-0000-0000-000000009999"
        release = self.a / "mbdump-release.tsv"
        rows = release.read_text().splitlines()
        fields = rows[1].split("\t")
        fields[1] = orphan
        rows[1] = "\t".join(fields)
        release.write_text("\n".join(rows) + "\n")
        self.config["fk_bad"] = orphan
        self.assert_success(self.run_import())
        chunks = self.events("chunks")
        artist_poison = "".join(c["poison"] for c in chunks if c["table"] == "artist")
        release_poison = "".join(c["poison"] for c in chunks if c["table"] == "release")
        self.assertEqual(artist_poison, "bad_uuid\t3\t" + bad.rstrip("\n").replace("\t", r"\t") + "\n")
        self.assertEqual(release_poison, "fk_missing_artist\t2\t" + rows[1].replace("\\", "\\\\").replace("\t", r"\t") + "\n")
        self.assertEqual(sum(len(c["valid"].splitlines()) for c in chunks if c["table"] == "release"), 1)
        self.assertEqual(len(self.state()["dlq"]), 2)

    def test_interrupted_track_tail_resumes_without_replaying_or_early_prune(self):
        self.config["fail_track_tail"] = True
        # Three track rows force two chunks; three linked rows in each table.
        result = self.run_import(sequence="100", directory=self.b)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("injected validator failure on track tail", result.stderr)
        first = self.events("chunks")
        tracks = [c for c in first if c["table"] == "track"]
        self.assertEqual([(c["offset"], c["end"]) for c in tracks], [(0, 2)])
        self.assertNotIn("DELETE FROM mb_track", tracks[0]["sql"], "unfinished track tail was pruned")
        self.assertEqual(self.state()["last_packet"], 41, "interrupted import seeded replication state")
        self.config.clear()
        self.assert_success(self.run_import(sequence="100", directory=self.b))
        resumed = self.events("chunks")[len(first):]
        self.assertEqual([(c["table"], c["offset"], c["end"]) for c in resumed], [("track", 2, 3)])
        self.assertIn("DELETE FROM mb_track", resumed[0]["sql"], "completed track tail never pruned")
        self.assert_loaded_publication(first + resumed, self.b)
        self.assertEqual(self.state()["last_packet"], 100)


def missing_case(table, shape):
    def test(self):
        path = self.a / f"mbdump-{table}.tsv"
        if shape == "missing":
            path.unlink()
        elif shape == "empty":
            path.write_text("")
        else:
            if os.geteuid() == 0:
                self.skipTest("unreadable-file control requires an unprivileged test process")
            path.chmod(0)
            self.addCleanup(path.chmod, 0o644)
            self.assertFalse(os.access(path, os.R_OK))
        before = self.state()
        self.assert_refused_without_writes(self.run_import(sequence="100"), before)
    return test


for table in TABLES:
    for shape in ("missing", "empty", "unreadable"):
        setattr(InputAdmission, f"test_preflight_{shape}_{table}_before_any_write", missing_case(table, shape))


def invalid_sequence_case(sequence):
    def test(self):
        before = self.state()
        self.assert_refused_without_writes(self.run_import(sequence=sequence), before)
    return test


for name, sequence in {
    "negative": "-1", "plus": "+1", "fraction": "1.5", "leading_space": " 7",
    "trailing_space": "7 ", "exponent": "1e3", "text": "nope",
    "integer_max_plus_one": "2147483648", "padded_out_of_range": "0002147483648",
    "unsigned_wrap_to_zero": "18446744073709551616", "long_digits": "9" * 200,
}.items():
    setattr(InputAdmission, f"test_sequence_{name}_refused_before_any_write", invalid_sequence_case(sequence))


def valid_sequence_case(sequence, expected):
    def test(self):
        self.assert_success(self.run_import(sequence=sequence))
        self.assertEqual(self.state()["last_packet"], expected)
        self.assert_loaded_publication(self.events("chunks"), self.a)
        self.assertEqual(self.state()["last_applied_at"], "2024-01-01")
        self.assertEqual(self.state()["lag_seconds"], 17)
        seeds = [e for e in self.events() if "UPDATE mb_import_state" in e["sql"]]
        self.assertEqual(len(seeds), int(expected > 41), "zero/older sequence must not attempt a state write")
    return test


for name, sequence, expected in (
    ("zero", "0", 41), ("older", "7", 41), ("equal", "41", 41), ("forward", "42", 42),
    ("integer_max", "2147483647", 2147483647), ("leading_zero", "00042", 42),
    ("long_zero_padding", "0" * 200 + "42", 42), ("empty_retains_unset_behavior", "", 41),
):
    setattr(InputAdmission, f"test_sequence_{name}_accepted", valid_sequence_case(sequence, expected))


if __name__ == "__main__":
    unittest.main(verbosity=2)
