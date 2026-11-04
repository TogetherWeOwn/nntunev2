#!/usr/bin/env python3
"""Real Git fixtures for migration lineage, coverage and fail-closed bounds."""
import contextlib
import copy
import hashlib
import io
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migration_history as history
import migrations_gate
import migrations_rename_check
import staging_api_schema_gate as staging

BODY = b"SET search_path = public, pg_catalog;\n" + b"-- fixture continuity\n" * 20 + b"SELECT 1;\n"


def manifest(entries):
    text = ""
    for entry in entries:
        text += "[[migration]]\n" + "".join(
            f'{key} = {json.dumps(entry[key])}\n' for key in ("file", "target", "sha256"))
        if entry.get("renamed_from"):
            text += "renamed_from = [" + ", ".join(
                "{ file = " + json.dumps(a["file"]) + ", sha256 = " + json.dumps(a["sha256"]) + " }"
                for a in entry["renamed_from"]) + "]\n"
        for edge in entry.get("source_rename_history", []):
            text += "[[migration.source_rename_history]]\n" + "".join(
                f'{key} = {json.dumps(value)}\n' for key, value in edge.items())
    return text


class GitFixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        (self.root / "migrations").mkdir()
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "user.name", "Migration Fixture")
        (self.root / "migrations/manifest.toml").write_text("")
        self.base = self.commit("base")

    def git(self, *args, check=True):
        return subprocess.run(["git", "-C", str(self.root), *args], check=check,
                              capture_output=True, text=True).stdout.strip()

    def commit(self, message="fixture"):
        self.git("add", "-A")
        self.git("commit", "-qm", message)
        return self.git("rev-parse", "HEAD")

    def write(self, name, body=BODY):
        (self.root / "migrations" / name).write_bytes(body)

    def rename(self, old, new, edit=False):
        self.git("mv", "migrations/" + old, "migrations/" + new)
        if edit:
            path = self.root / "migrations" / new
            path.write_bytes(path.read_bytes() + ("-- " + new + "\n").encode())
        self.commit("rename")

    def entries(self):
        return [{"file": p.name, "target": "app", "sha256": hashlib.sha256(p.read_bytes()).hexdigest()}
                for p in sorted((self.root / "migrations").glob("*.sql"))]

    def prepare(self):
        git_history = history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD"))
        states = git_history.collect()
        entries = self.entries()
        for entry in entries:
            events = states["migrations/" + entry["file"]][1]
            entry["source_rename_history"] = [dict(zip(history.EDGE_KEYS, e)) for e in sorted(events)]
            aliases = {e[2].removeprefix("migrations/") for e in events
                       if e[4] == e[5] == entry["sha256"]}
            entry["renamed_from"] = [{"file": name, "sha256": entry["sha256"]} for name in sorted(aliases)]
        return entries

    def verify(self, entries, coverage=None):
        return history.verify(self.root, self.base, self.git("rev-parse", "HEAD"),
                              manifest(entries), coverage)

    def chain(self):
        # Same final ledger/crypto shapes as the reported chains; all SQL is
        # independently authored fixture content, never copied application SQL.
        for suffix, start, final in (("posted_files", 27, 35), ("posted_files_crypto", 28, 36)):
            old = f"{start:04d}_{suffix}.sql"
            self.write(old)
            self.commit("create")
            for number in (start + 1, final):
                new = f"{number:04d}_{suffix}.sql"
                self.rename(old, new, edit=True)
                old = new
                self.write(old, (self.root / "migrations" / old).read_bytes() + b"-- intervening edit\n")
                self.commit("ordinary edit")
        return self.prepare()

    def test_one_hop_alias_is_genuine_and_required(self):
        self.write("0001_old.sql")
        self.commit()
        self.rename("0001_old.sql", "0002_final.sql")
        entries = self.prepare()
        # A genuine alias alone retains one-hop behavior; history is optional.
        entries[0]["source_rename_history"] = []
        self.verify(entries)
        entries[0]["renamed_from"] = []
        with self.assertRaisesRegex(history.HistoryError, "lacks renamed_from"):
            self.verify(entries)

    def test_changed_chain_passes_through_final_names(self):
        entries = self.chain()
        verified = self.verify(entries)
        self.assertEqual([len(edges) for edges in verified.values()], [2, 2])
        for entry in entries:
            self.assertEqual(entry["renamed_from"], [])
            for index in range(2):
                broken = copy.deepcopy(entries)
                del broken[entries.index(entry)]["source_rename_history"][index]
                with self.assertRaisesRegex(history.HistoryError, "lacks source_rename_history"):
                    self.verify(broken)
        # Input metadata order never decides graph traversal or output order.
        permuted = copy.deepcopy(entries)
        for entry in permuted:
            entry["source_rename_history"].reverse()
        self.assertEqual(self.verify(permuted), verified)

    def test_staging_never_accepts_source_history(self):
        entries = self.chain()
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        expected = staging.load_expected(path)
        rows = [(e["file"], e["sha256"]) for e in entries]
        self.assertTrue(staging.compare(expected, rows))
        for entry in entries:
            for edge in entry["source_rename_history"]:
                for side in ("old", "new"):
                    pair = (edge[side + "_path"].removeprefix("migrations/"), edge[side + "_sha256"])
                    if pair in rows:
                        continue
                    self.assertFalse(staging.compare(expected, [pair]))
                    self.assertFalse(staging.compare(expected, rows + [pair]))

    def test_equal_hash_fabricated_alias_fails_provenance(self):
        entries = self.chain()
        entry = entries[0]
        name = entry["source_rename_history"][0]["old_path"].removeprefix("migrations/")
        entry["renamed_from"] = [{"file": name, "sha256": entry["sha256"]}]
        with self.assertRaisesRegex(history.HistoryError, "genuine byte-identical"):
            self.verify(entries)

    def test_forged_fields_and_missing_history_refuse(self):
        entries = self.chain()
        for field in history.EDGE_KEYS:
            broken = copy.deepcopy(entries)
            value = ("0" * 40 if field in ("parent", "commit") else "0" * 64
                     if field.endswith("sha256") else "migrations/0099_forged.sql")
            broken[0]["source_rename_history"][0][field] = value
            with self.subTest(field=field), self.assertRaises(history.HistoryError):
                self.verify(broken)
        broken = copy.deepcopy(entries)
        broken[0]["source_rename_history"] = []
        with self.assertRaises(history.HistoryError):
            self.verify(broken)

    def test_deleted_terminal_is_not_suppressed(self):
        self.write("0001_old.sql")
        self.commit()
        self.rename("0001_old.sql", "0002_gone.sql", edit=True)
        self.git("rm", "migrations/0002_gone.sql")
        self.commit()
        with self.assertRaisesRegex(history.HistoryError, "no provable rename lineage"):
            self.verify([])

    def test_reused_name_is_independent_not_a_basename_graph(self):
        self.write("0001_old.sql")
        self.commit()
        self.rename("0001_old.sql", "0002_first.sql", edit=True)
        self.write("0001_old.sql", BODY + b"-- unrelated second creation\n")
        self.commit()
        self.rename("0001_old.sql", "0003_second.sql", edit=True)
        entries = self.prepare()
        with self.assertRaisesRegex(history.HistoryError, "ambiguous entry"):
            self.verify(entries)

    def test_independent_branch_lineages_cannot_unify_at_merge(self):
        self.git("checkout", "-qb", "left")
        self.write("0001_old.sql")
        self.commit()
        self.rename("0001_old.sql", "0003_final.sql", edit=True)
        self.git("checkout", "-qb", "right", self.base)
        self.write("0002_other.sql", BODY + b"-- right birth\n")
        self.commit()
        self.rename("0002_other.sql", "0003_final.sql", edit=True)
        self.git("merge", "--no-commit", "left", check=False)
        self.write("0003_final.sql", BODY + b"-- explicit conflict resolution\n")
        self.commit("merge")
        with self.assertRaisesRegex(history.HistoryError, "independent Git lineages"):
            self.verify(self.prepare())

    def test_ordinary_main_merge_preserves_single_source_lineage(self):
        self.git("checkout", "-qb", "feature")
        self.write("0001_old.sql")
        self.commit()
        self.rename("0001_old.sql", "0002_final.sql", edit=True)
        self.git("checkout", "main")
        (self.root / "readme").write_text("unrelated main change")
        self.commit()
        self.git("checkout", "feature")
        self.git("merge", "--no-edit", "main")
        self.verify(self.prepare())

    def test_unreachable_commit_and_truncated_git_history_refuse(self):
        entries = self.chain()
        self.git("checkout", "-qb", "unreachable", self.base)
        (self.root / "orphan").write_text("unrelated")
        unrelated = self.commit()
        self.git("checkout", "main")
        entries[0]["source_rename_history"][0]["commit"] = unrelated
        with self.assertRaises(history.HistoryError):
            self.verify(entries)
        # A real parent hidden by a shallow boundary cannot look like a birth.
        git_dir = pathlib.Path(self.git("rev-parse", "--absolute-git-dir"))
        (git_dir / "shallow").write_text(self.git("rev-parse", "HEAD") + "\n")
        with self.assertRaises(history.HistoryError):
            self.verify(self.entries())

    def test_alias_and_source_deletion_fail_base_drift(self):
        entries = self.chain()
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        base = self.commit("record history")
        entries[0]["source_rename_history"].pop()
        path.write_text(manifest(entries))
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertGreater(migrations_gate.check_drift(self.root, base), 0)
        self.assertIn("source-history-removed", output.getvalue())

    def test_source_metadata_does_not_allow_base_sql_edits_or_removal(self):
        entries = self.chain()
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        base = self.commit("applied base")
        sql = self.root / "migrations" / entries[0]["file"]
        sql.write_bytes(sql.read_bytes() + b"SELECT 9;\n")
        entries[0]["sha256"] = hashlib.sha256(sql.read_bytes()).hexdigest()
        path.write_text(manifest(entries))
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertGreater(migrations_gate.check_drift(self.root, base), 0)
        sql.unlink()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertGreater(migrations_gate.check_drift(self.root, base), 0)

    def test_all_collection_bounds_refuse(self):
        entries = self.chain()
        for key, limit in (("MAX_COMMITS", 1), ("MAX_EVENTS", 1), ("MAX_FILES", 1),
                           ("MAX_STATES", 1), ("MAX_EVENT_REFERENCES", 1),
                           ("MAX_TOTAL_OUTPUT", 1), ("MAX_BLOB", 1), ("MAX_OUTPUT", 1),
                           ("GIT_TIMEOUT", 0)):
            # Default argument limits are pinned when functions are defined;
            # exercise explicit Git output bound directly for MAX_OUTPUT.
            with self.subTest(key=key), mock.patch.object(history, key, limit):
                with self.assertRaises(history.HistoryError):
                    if key == "MAX_OUTPUT":
                        h = history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD"))
                        h.git("show", "HEAD:migrations/0035_posted_files.sql", limit=1)
                    else:
                        self.verify(entries)

    def test_git_path_and_object_type_boundaries_refuse(self):
        for name, kind in (("nested/0001_bad.sql", "nested"),
                           ("0001_link.sql", "link"), ("0001_exec.sql", "executable")):
            with self.subTest(kind=kind):
                self.git("reset", "--hard", self.base)
                path = self.root / "migrations" / name
                path.parent.mkdir(exist_ok=True)
                if kind == "link":
                    path.symlink_to("manifest.toml")
                else:
                    path.write_bytes(BODY)
                    if kind == "executable":
                        path.chmod(0o755)
                head = self.commit()
                with self.assertRaises(history.HistoryError):
                    history.GitHistory(self.root, self.base, head).collect()

    def test_low_similarity_delete_add_is_not_hidden_history(self):
        self.write("0001_old.sql")
        self.commit()
        self.git("rm", "migrations/0001_old.sql")
        self.write("0002_new.sql", b"SELECT 9;\n")
        self.commit()
        with self.assertRaisesRegex(history.HistoryError, "no provable rename"):
            history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD")).collect()

    def test_manifest_loaders_reject_differing_applied_alias(self):
        entries = self.chain()
        entries[0]["renamed_from"] = [{"file": "0001_old.sql", "sha256": "a" * 64}]
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        with self.assertRaises(staging.GateError):
            staging.load_expected(path)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertGreater(migrations_gate.check_manifest(self.root), 0)

    def test_missing_and_malformed_git_transport_refuse(self):
        entries = self.chain()
        h = history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD"))
        for raw in (b"R100\\0migrations/0001_old.sql\\0", b"M\\0migrations/0001_old.sql",
                    b"X\\0migrations/0001_old.sql\\0"):
            with mock.patch.object(h, "git", return_value=raw):
                with self.assertRaises(history.HistoryError):
                    h.diff("a" * 40, "b" * 40)
        with self.assertRaises(history.HistoryError):
            history.GitHistory(self.root, "0" * 40, self.git("rev-parse", "HEAD"))
        with self.assertRaises(history.HistoryError):
            history.GitHistory(self.root, "main", self.git("rev-parse", "HEAD"))

    def test_final_checksum_is_independently_verified(self):
        entries = self.chain()
        entries[0]["sha256"] = "0" * 64
        with self.assertRaisesRegex(history.HistoryError, "final migration bytes"):
            self.verify(entries)

    def test_cli_requires_complete_pinned_net_coverage(self):
        entries = self.chain()
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        self.commit("record source metadata")
        head = self.git("rev-parse", "HEAD")
        metadata = {"base": {"sha": self.base}, "head": {"sha": head}, "changed_files": 3}
        files = [{"filename": "migrations/manifest.toml", "status": "modified"}] + [
            {"filename": "migrations/" + e["file"], "status": "added"} for e in entries]
        (self.root / "files.json").write_text(json.dumps([files]))
        (self.root / "metadata.json").write_text(json.dumps(metadata))
        args = ["--root", str(self.root), "--base-sha", self.base, "--head-sha", head,
                "--files-file", str(self.root / "files.json"),
                "--metadata-file", str(self.root / "metadata.json")]
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(migrations_rename_check.main(args), 0)
        # Deleting an API migration record cannot be hidden by valid Git edges.
        (self.root / "files.json").write_text(json.dumps([files[:-1]]))
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(migrations_rename_check.main(args), 1)

    def test_duplicate_net_and_git_coverage_is_idempotent(self):
        self.write("0001_old.sql")
        self.commit()
        # Base old -> final API mapping and the actual log edge overlap.
        self.base = self.git("rev-parse", "HEAD")
        (self.root / "migrations/manifest.toml").write_text(manifest(self.entries()))
        self.base = self.commit("base entry")
        # Base migration immutability is a separate gate; use API overlap on
        # the lineage collector itself, not verify (which forbids base removal).
        self.rename("0001_old.sql", "0002_final.sql", edit=True)
        h = history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD"))
        states = h.collect()
        self.assertEqual(len(h.events), 1)
        self.assertEqual(len(states["migrations/0002_final.sql"][1]), 1)
        self.assertEqual(h.collect(), states)


class StrictInputs(unittest.TestCase):
    def test_history_schema_and_target_confusion(self):
        edge = dict(zip(history.EDGE_KEYS, ("a" * 40, "b" * 40, "migrations/0001_old.sql",
                        "migrations/0002_new.sql", "c" * 64, "d" * 64)))
        base = {"file": "0002_new.sql", "target": "app", "sha256": "d" * 64,
                "source_rename_history": [edge]}
        for field, bad in (("old_path", "migrations/../0001_old.sql"), ("new_path", "other/0002_new.sql"),
                           ("old_sha256", "C" * 64), ("commit", "main"), ("parent", None)):
            entry = copy.deepcopy(base)
            entry["source_rename_history"][0][field] = bad
            with self.subTest(field=field), self.assertRaises(history.HistoryError):
                history.parse_manifest(manifest([entry]))
        for malformed in ("history", [None], [dict(edge, extra="x")], [edge, edge]):
            with self.assertRaises(history.HistoryError):
                history.validate_history([dict(base, source_rename_history=malformed)])
        mirror = {"file": "0001_old.sql", "target": "mirror", "sha256": "c" * 64}
        with self.assertRaisesRegex(history.HistoryError, "target ownership"):
            history.parse_manifest(manifest([base, mirror]))

    def test_api_types_counts_paths_pagination_and_bounds(self):
        base, head = "a" * 40, "b" * 40
        metadata = {"base": {"sha": base}, "head": {"sha": head}, "changed_files": 1}
        item = {"filename": "migrations/0002_final.sql", "status": "added"}
        def parse(pages, meta=metadata):
            return history.api_coverage(json.dumps(pages), json.dumps(meta), base, head)
        parse([[item]])
        for pages in ([], {}, [[item], []], [[item, item]], [[None]], [[{}]],
                      [[dict(item, filename="migrations/../bad.sql")]],
                      [[dict(item, status="renamed")]], [[dict(item, previous_filename="old")]]):
            with self.subTest(pages=pages), self.assertRaises(history.HistoryError):
                parse(pages)
        for field in (True, -1, 3000, 2, None):
            with self.subTest(count=field), self.assertRaises(history.HistoryError):
                parse([[item]], dict(metadata, changed_files=field))
        with mock.patch.object(history, "MAX_INPUT", 1):
            with self.assertRaises(history.HistoryError):
                parse([[item]])
            with self.assertRaises(history.HistoryError):
                history.parse_manifest("migration=[]")
        with self.assertRaises(history.HistoryError):
            history.api_coverage("[", "{}", base, head)
        with self.assertRaises(history.HistoryError):
            history.api_coverage('[[{"filename":"a","filename":"b"}]]', json.dumps(metadata), base, head)
        with tempfile.TemporaryDirectory() as tmp:
            path = pathlib.Path(tmp) / "input"
            path.write_text("abcd")
            with mock.patch.object(history, "MAX_INPUT", 1), self.assertRaises(history.HistoryError):
                history.read_input(path)
            with self.assertRaises(history.HistoryError):
                history.read_input(path.with_name("missing"))
        with self.assertRaises(history.HistoryError):
            history.parse_manifest('source_rename_history = []')
        with self.assertRaises(history.HistoryError):
            parse([[item]], dict(metadata, head={"sha": base}))


if __name__ == "__main__":
    unittest.main()
