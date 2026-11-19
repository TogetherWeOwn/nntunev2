#!/usr/bin/env python3
"""Real Git fixtures for migration lineage, coverage and fail-closed bounds."""
import contextlib
import copy
import hashlib
import io
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
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

    def source_witness(self, entries):
        text = manifest(entries)
        (self.root / "migrations/manifest.toml").write_text(text)
        head = self.commit("source manifest")
        return head, hashlib.sha256(text.encode()).hexdigest()

    def test_preserved_source_reverifies_history_with_shared_budget(self):
        entries = self.chain()
        head, digest = self.source_witness(entries)
        budget = history.EvidenceBudget()
        self.assertEqual(history.verify_preserved_source(
            self.root, self.base, head, digest, budget=budget), history.parse_manifest(manifest(entries)))
        self.assertGreater(budget.total_output, 0)
        with self.assertRaisesRegex(history.HistoryError, "output bound"):
            history.verify_preserved_source(self.root, self.base, head, digest,
                                            budget=history.EvidenceBudget(
                                                total_output=history.MAX_TOTAL_OUTPUT))
        with self.assertRaisesRegex(history.HistoryError, "deadline"):
            history.verify_preserved_source(self.root, self.base, head, digest,
                                            budget=history.EvidenceBudget(deadline=0))
        with self.assertRaisesRegex(history.HistoryError, "digest mismatch"):
            history.verify_preserved_source(self.root, self.base, head, "0" * 64,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_rejects_deadline_expired_during_final_validation(self):
        self.write("0001_original.sql")
        head, digest = self.source_witness(self.entries())
        budget = history.EvidenceBudget()
        monotonic = history.time.monotonic
        original_sha = history.GitHistory.sha
        expired = False

        def sha_then_expire(git_history, commit, path):
            nonlocal expired
            result = original_sha(git_history, commit, path)
            expired = True
            return result

        def clock():
            return budget.deadline + 1 if expired else monotonic()

        with mock.patch.object(history.time, "monotonic", side_effect=clock):
            with mock.patch.object(history.GitHistory, "sha", sha_then_expire):
                with self.assertRaisesRegex(history.HistoryError, "deadline"):
                    history.verify_preserved_source(self.root, self.base, head, digest, budget=budget)
        self.assertTrue(expired)

    def test_preserved_source_rejects_unverified_baseline_alias(self):
        self.write("0002_final.sql")
        entries = self.entries()
        entries[0]["renamed_from"] = [{"file": "0001_old.sql", "sha256": entries[0]["sha256"]}]
        text = manifest(entries)
        (self.root / "migrations/manifest.toml").write_text(text)
        base = self.commit("fabricated baseline")
        self.write("0003_later.sql")
        head, digest = self.source_witness(entries + [self.entries()[1]])
        with self.assertRaisesRegex(history.HistoryError, "unverified baseline provenance"):
            history.verify_preserved_source(self.root, base, head, digest,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_charges_prior_shared_budget_use(self):
        self.write("0001_original.sql")
        head, digest = self.source_witness(self.entries())
        budget = history.EvidenceBudget()
        history.verify_preserved_source(self.root, self.base, head, digest, budget=budget)
        self.assertGreater(budget.total_output, 0)
        with mock.patch.object(history, "MAX_TOTAL_OUTPUT", budget.total_output):
            with self.assertRaisesRegex(history.HistoryError, "output bound"):
                history.verify_preserved_source(self.root, self.base, head, digest, budget=budget)

    def test_preserved_source_rejects_unverified_claims_and_missing_objects(self):
        entries = self.chain()
        entries[0]["source_rename_history"].pop()
        head, digest = self.source_witness(entries)
        with self.assertRaisesRegex(history.HistoryError, "lacks source_rename_history"):
            history.verify_preserved_source(self.root, self.base, head, digest,
                                            budget=history.EvidenceBudget())
        with self.assertRaises(history.HistoryError):
            history.verify_preserved_source(self.root, self.base, "0" * 40, digest,
                                            budget=history.EvidenceBudget())
        with self.assertRaisesRegex(history.HistoryError, "ancestor"):
            history.verify_preserved_source(self.root, head, self.base, digest,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_requires_original_graph_and_data_objects(self):
        self.git("checkout", "-qb", "side")
        self.git("commit", "--allow-empty", "-qm", "side parent")
        side = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "main")
        self.git("commit", "--allow-empty", "-qm", "intermediate")
        intermediate = self.git("rev-parse", "HEAD")
        self.git("merge", "--no-ff", "-qm", "merge", "side")
        self.write("0001_original.sql")
        head, digest = self.source_witness(self.entries())
        expected = history.parse_manifest(manifest(self.entries()))
        self.assertEqual(history.verify_preserved_source(
            self.root, self.base, head, digest, budget=history.EvidenceBudget()), expected)

        tree = self.git("rev-parse", f"{head}^{{tree}}")
        blob = self.git("rev-parse", f"{head}:migrations/0001_original.sql")
        # Remove one loose object at a time, restoring it before the next case.
        # The intact graph above is the positive control for all failures.
        for oid in (intermediate, side, tree, blob):
            with self.subTest(missing=oid):
                obj = self.root / ".git/objects" / oid[:2] / oid[2:]
                data = obj.read_bytes()
                try:
                    obj.unlink()
                    with self.assertRaises(history.HistoryError):
                        history.verify_preserved_source(
                            self.root, self.base, head, digest, budget=history.EvidenceBudget())
                finally:
                    obj.write_bytes(data)

    def test_preserved_source_rejects_mismatched_final_sql(self):
        self.write("0001_original.sql")
        entries = self.entries()
        entries[0]["sha256"] = "0" * 64
        head, digest = self.source_witness(entries)
        with self.assertRaisesRegex(history.HistoryError, "final migration bytes"):
            history.verify_preserved_source(self.root, self.base, head, digest,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_rejects_shallow_repository(self):
        self.write("0001_original.sql")
        head, digest = self.source_witness(self.entries())
        (self.root / ".git/shallow").write_text(self.base + "\n")
        with self.assertRaisesRegex(history.HistoryError, "shallow source repository"):
            history.verify_preserved_source(self.root, self.base, head, digest,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_rejects_nonregular_manifest(self):
        path = self.root / "migrations/manifest.toml"
        path.unlink()
        path.symlink_to("missing")
        head = self.commit("symlink manifest")
        with self.assertRaisesRegex(history.HistoryError, "regular non-executable"):
            history.verify_preserved_source(self.root, self.base, head, "0" * 64,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_rejects_executable_and_oversized_manifest(self):
        path = self.root / "migrations/manifest.toml"
        path.chmod(0o755)
        executable = self.commit("executable manifest")
        with self.assertRaisesRegex(history.HistoryError, "regular non-executable"):
            history.verify_preserved_source(self.root, self.base, executable, "0" * 64,
                                            budget=history.EvidenceBudget())
        path.chmod(0o644)
        path.write_bytes(b"#" + b"x" * history.MAX_INPUT)
        oversized = self.commit("oversized manifest")
        with self.assertRaisesRegex(history.HistoryError, "output bound"):
            history.verify_preserved_source(self.root, self.base, oversized, "0" * 64,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_rejects_non_utf8_manifest(self):
        raw = b"# invalid manifest\n\xff"
        (self.root / "migrations/manifest.toml").write_bytes(raw)
        head = self.commit("non-UTF-8 manifest")
        with self.assertRaisesRegex(history.HistoryError, "not UTF-8"):
            history.verify_preserved_source(
                self.root, self.base, head, hashlib.sha256(raw).hexdigest(),
                budget=history.EvidenceBudget())

    def test_preserved_source_ignores_git_replacement_objects(self):
        self.write("0001_original.sql")
        entries = self.entries()
        head, digest = self.source_witness(entries)
        replacement_text = "# replacement manifest\n"
        (self.root / "migrations/manifest.toml").write_text(replacement_text)
        replacement = self.commit("replacement")
        self.git("replace", head, replacement)
        self.assertEqual(history.verify_preserved_source(
            self.root, self.base, head, digest, budget=history.EvidenceBudget()), entries)
        replacement_digest = hashlib.sha256(replacement_text.encode()).hexdigest()
        with self.assertRaisesRegex(history.HistoryError, "digest mismatch"):
            history.verify_preserved_source(self.root, self.base, head, replacement_digest,
                                            budget=history.EvidenceBudget())

    def test_preserved_source_requires_shared_budget_and_digest(self):
        with self.assertRaisesRegex(history.HistoryError, "shared evidence budget"):
            history.verify_preserved_source(self.root, self.base, self.base, "0" * 64,
                                            budget=None)
        with self.assertRaisesRegex(history.HistoryError, "invalid source manifest digest"):
            history.verify_preserved_source(self.root, self.base, self.base, "A" * 64,
                                            budget=history.EvidenceBudget())

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

    def test_byte_identical_roundtrip_uses_current_identity_not_self_alias(self):
        self.write("0001_first.sql")
        self.commit()
        self.rename("0001_first.sql", "0002_intermediate.sql")
        self.rename("0002_intermediate.sql", "0001_first.sql")
        entries = self.prepare()
        entries[0]["renamed_from"] = [{"file": "0002_intermediate.sql",
                                        "sha256": entries[0]["sha256"]}]
        self.assertEqual(len(self.verify(entries)["0001_first.sql"]), 2)

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

    def diverge_target(self, entries):
        self.git("checkout", "-qb", "feature")
        (self.root / "feature.txt").write_text("unrelated PR change")
        self.commit("feature")
        self.git("checkout", "main")
        (self.root / "target.txt").write_text("unrelated target change")
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        target = self.commit("target-only manifest changes")
        self.git("checkout", "feature")
        return target

    def test_stale_head_ignores_target_only_migration(self):
        self.write("0001_shared.sql")
        entries = self.entries()
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        self.base = self.commit("common baseline")
        self.write("0002_target.sql", BODY + b"-- target only\n")
        target_entries = self.entries()
        # Keep the target-only SQL off the feature branch as well as its manifest.
        (self.root / "migrations/manifest.toml").write_text(manifest(target_entries))
        target = self.commit("target migration")
        self.git("checkout", "-qb", "feature", self.base)
        (self.root / "feature.txt").write_text("unrelated PR change")
        self.commit("feature")
        self.assertNotEqual(target, self.base)
        self.base = target
        self.assertEqual(self.verify(entries, ({}, set())), {"0001_shared.sql": []})

    def test_stale_head_ignores_target_only_alias_and_source_additions(self):
        entries = self.chain()
        path = self.root / "migrations/manifest.toml"
        # The common baseline predates the already-verified metadata additions.
        baseline = copy.deepcopy(entries)
        for entry in baseline:
            entry["source_rename_history"] = []
        path.write_text(manifest(baseline))
        self.base = self.commit("common baseline")
        target_entries = copy.deepcopy(entries)
        target_entries[0]["renamed_from"] = [{"file": "0020_target_alias.sql",
                                               "sha256": entries[0]["sha256"]}]
        self.base = self.diverge_target(target_entries)
        self.assertEqual(self.verify(baseline), {e["file"]: [] for e in baseline})

    def test_diverged_head_cannot_delete_common_alias_or_source_history(self):
        entries = self.chain()
        entries[0]["renamed_from"] = [{"file": "0020_applied_alias.sql",
                                       "sha256": entries[0]["sha256"]}]
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        self.base = self.commit("record verified common history")
        self.base = self.diverge_target(entries)
        for field in ("renamed_from", "source_rename_history"):
            broken = copy.deepcopy(entries)
            broken[0][field] = []
            with self.subTest(field=field), self.assertRaisesRegex(
                    history.HistoryError, "append-only against base"):
                self.verify(broken)

    def test_diverged_head_cannot_remove_common_manifest_entry(self):
        self.write("0001_shared.sql")
        entries = self.entries()
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        self.base = self.commit("common baseline")
        self.base = self.diverge_target(entries)
        with self.assertRaisesRegex(history.HistoryError, "base migration entry removed"):
            self.verify([])

    def test_diverged_head_cannot_edit_or_delete_common_sql(self):
        self.write("0001_shared.sql")
        entries = self.entries()
        path = self.root / "migrations/manifest.toml"
        path.write_text(manifest(entries))
        common = self.commit("common baseline")
        self.base = self.diverge_target(entries)
        sql = self.root / "migrations/0001_shared.sql"
        sql.write_bytes(BODY + b"SELECT 9;\n")
        entries = self.entries()
        path.write_text(manifest(entries))
        self.commit("PR changes common SQL and hash")
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertGreater(migrations_gate.check_drift(self.root, common), 0)
        self.assertIn("applied-edited", output.getvalue())
        sql.unlink()
        path.write_text("")
        self.commit("PR deletes common SQL")
        with contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertGreater(migrations_gate.check_drift(self.root, common), 0)
        self.assertIn("applied-removed", output.getvalue())
        with self.assertRaisesRegex(history.HistoryError, "no provable rename lineage"):
            self.verify([])

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

    def test_collector_file_and_event_bounds_refuse_directly(self):
        self.chain()
        parent = self.git("rev-parse", "HEAD")
        for name in ("first.txt", "second.txt"):
            (self.root / name).write_text("non-migration change")
        head = self.commit("non-migration changes")
        h = history.GitHistory(self.root, self.base, head)
        with mock.patch.object(history, "MAX_FILES", 1):
            with self.assertRaisesRegex(history.HistoryError, "Git migration file bound exceeded"):
                h.tree(head)
            self.assertNotIn(head, h.trees)
            # Count transport records even when none is a migration record.
            with self.assertRaisesRegex(history.HistoryError, "Git change record bound exceeded"):
                h.diff(parent, head)
            self.assertNotIn((parent, head, True), h.diffs)
        with mock.patch.object(history, "MAX_EVENTS", 1):
            with self.assertRaisesRegex(history.HistoryError, "Git rename event bound exceeded"):
                h.collect()
            self.assertEqual(len(h.events), 2)

    def test_manifest_file_and_event_bounds_refuse(self):
        entries = self.chain()
        for key, diagnostic in (("MAX_FILES", "migration entry bound exceeded"),
                                ("MAX_EVENTS", "source rename event bound exceeded")):
            with self.subTest(key=key), mock.patch.object(history, key, 1):
                with self.assertRaisesRegex(history.HistoryError, diagnostic):
                    history.parse_manifest(manifest(entries))

    def test_all_collection_bounds_refuse(self):
        entries = self.chain()
        for key, limit in (("MAX_COMMITS", 1),
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
        self.chain()
        h = history.GitHistory(self.root, self.base, self.git("rev-parse", "HEAD"))
        for raw, diagnostic in (
                (b"R100\0migrations/0001_old.sql\0", "truncated Git change"),
                (b"M\0migrations/0001_old.sql", "truncated Git diff"),
                (b"X\0migrations/0001_old.sql\0", "unsupported Git change type")):
            with self.subTest(diagnostic=diagnostic), mock.patch.object(h, "git", return_value=raw):
                with self.assertRaisesRegex(history.HistoryError, diagnostic):
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

    def full_metadata(self, metadata):
        result = copy.deepcopy(metadata)
        result.setdefault("number", 1)
        for side in ("base", "head"):
            result[side].setdefault("ref", "main" if side == "base" else "feature")
            result[side].setdefault("repo", {"id": 1, "full_name": "example/project"})
        return result

    def event(self, metadata):
        metadata = self.full_metadata(metadata)
        metadata = {key: metadata[key] for key in ("number", "base", "head", "changed_files")}
        return {"number": 1, "repository": metadata["base"]["repo"], "pull_request": metadata}

    def run_workflow(self, pages, metadata, api_exit=0, *, samples=None, event=None,
                     missing_code=False, poison=False):
        from test_ci_pnpm_retry import split_jobs, split_steps, step_field

        source = pathlib.Path(__file__).resolve().parents[2]
        job = split_jobs((source / ".github/workflows/migrations-staging.yml").read_text())["migrations-staging"]
        steps = split_steps(job)
        command = "\n".join(textwrap.dedent(next(
            step for step in steps if step_field(step, "name") == name
        ).split("        run: |\n", 1)[1]) for name in (
            "Collect pinned PR API evidence", "Check migration rename history"))
        workspace_tmp = tempfile.TemporaryDirectory()
        self.addCleanup(workspace_tmp.cleanup)
        workspace = pathlib.Path(workspace_tmp.name)
        (workspace / "pr-data").symlink_to(self.root, target_is_directory=True)
        scripts = workspace / "trusted-checker/.github/scripts"
        scripts.mkdir(parents=True)
        for name in ("migration_history.py", "migration_snapshot.py", "migrations_rename_check.py"):
            if not missing_code:
                shutil.copyfile(source / ".github/scripts" / name, scripts / name)
        api = workspace / "api"
        api.mkdir()
        self.workflow_api = api
        if poison:
            bad = "raise RuntimeError('PR-controlled Python must not load')\n"
            for name in ("migration_snapshot.py", "migration_history.py", "json.py", "sitecustomize.py"):
                (self.root / name).write_text(bad)
        (api / "files.json").write_text(json.dumps(pages))
        metadata = self.full_metadata(metadata)
        if event is None:
            event = self.event(metadata)
            event["pull_request"]["base"]["sha"] = self.base
            event["pull_request"]["head"]["sha"] = self.git("rev-parse", "HEAD")
        (api / "event.json").write_text(json.dumps(event))
        (api / "samples.json").write_text(json.dumps(
            [self.full_metadata(m) for m in samples] if samples else [metadata]))
        # Exercise the actual workflow shell/isolated imports, replacing only network.
        stub = api / "gh"
        stub.write_text(
            "#!/usr/bin/env python3\nimport os, pathlib, sys, json\n"
            "root = pathlib.Path(os.environ['RUNNER_TEMP'])\n"
            "files = '/files?' in sys.argv[-1]\n"
            "if files:\n"
            "    pages = json.loads((root / 'files.json').read_text())\n"
            "    page = int(sys.argv[-1].rsplit('page=', 1)[1])\n"
            "    print(json.dumps(pages[page - 1] if page <= len(pages) else []))\n"
            "else:\n"
            "    counter = root / 'calls'\n"
            "    n = int(counter.read_text()) if counter.exists() else 0\n"
            "    samples = json.loads((root / 'samples.json').read_text())\n"
            "    print(json.dumps(samples[min(n, len(samples) - 1)]))\n"
            "    counter.write_text(str(n + 1))\n"
            "sys.exit(int(os.environ['API_EXIT']) if files else 0)\n"
        )
        stub.chmod(0o755)
        env = {**os.environ, "PATH": str(api) + os.pathsep + os.environ["PATH"],
               "BASH_ENV": "/dev/null", "RUNNER_TEMP": str(api), "REPO": "example/project", "PR_NUMBER": "1",
               "BASE_SHA": self.base, "HEAD_SHA": self.git("rev-parse", "HEAD"),
               "GITHUB_WORKSPACE": str(workspace), "GITHUB_EVENT_PATH": str(api / "event.json"),
               "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null",
               "GIT_CONFIG_COUNT": "2", "GIT_CONFIG_KEY_0": "core.hooksPath",
               "GIT_CONFIG_VALUE_0": "/dev/null", "GIT_CONFIG_KEY_1": "core.fsmonitor",
               "GIT_CONFIG_VALUE_1": "false", "API_EXIT": str(api_exit)}
        return subprocess.run(["bash", "-eo", "pipefail", "-c", command], cwd=api,
                              env=env, capture_output=True, text=True, timeout=30)

    def workflow_inputs(self):
        entries = self.chain()
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        head = self.commit("record source metadata")
        files = [{"filename": "migrations/manifest.toml", "status": "modified"}] + [
            {"filename": "migrations/" + e["file"], "status": "added"} for e in entries]
        files += [{"filename": f"docs/file-{i}.md", "status": "modified"} for i in range(97)]
        files += [{"filename": "docs/new.md", "previous_filename": "docs/old.md", "status": "renamed"}]
        metadata = {"base": {"sha": self.base}, "head": {"sha": head}, "changed_files": len(files)}
        return [files[:100], files[100:]], metadata

    def test_workflow_projects_large_patches_retaining_complete_pages_and_history(self):
        pages, metadata = self.workflow_inputs()
        pages[0][0]["patch"] = "x" * (history.MAX_INPUT + 1)
        metadata["body"] = "y" * (history.MAX_INPUT + 1)
        result = self.run_workflow(pages, metadata)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        self.assertIn("pinned Git source lineage and applied aliases verified", result.stdout)
        compact = history.read_input(self.workflow_api / "pr-files.json")
        projected = json.loads(compact)
        self.assertEqual([100, 1], [len(page) for page in projected])
        self.assertEqual(pages[-1], projected[-1])  # Includes the non-migration rename.
        self.assertEqual(set(pages[0][0]) - {"patch"}, set(projected[0][0]))
        self.assertNotIn("body", json.loads(history.read_input(self.workflow_api / "pr-metadata.json")))

    def test_workflow_projection_keeps_fail_closed_coverage_checks(self):
        pages, metadata = self.workflow_inputs()
        for mutation, diagnostic in (("truncated", "truncated or duplicated"),
                                     ("short-page", "truncated or duplicated"),
                                     ("missing-migration", "API migration coverage differs"),
                                     ("missing-old-path", "distinct old path"),
                                     ("stale-head", "pinned event commits"),
                                     ("file-cap", "file count missing or at truncation bound"),
                                     ("oversized-path", "input byte bound exceeded")):
            broken, meta = copy.deepcopy(pages), copy.deepcopy(metadata)
            if mutation == "truncated":
                broken.pop()
            elif mutation == "short-page":
                broken[-1].insert(0, broken[0].pop())
            elif mutation == "file-cap":
                meta["changed_files"] = history.MAX_FILES
            elif mutation == "oversized-path":
                broken[-1][-1]["filename"] = "docs/" + "x" * history.MAX_INPUT
            elif mutation == "missing-migration":
                broken[0][1] = {"filename": "docs/substitute.md", "status": "added"}
            elif mutation == "missing-old-path":
                del broken[-1][-1]["previous_filename"]
            else:
                meta["head"]["sha"] = self.base
            with self.subTest(mutation=mutation):
                result = self.run_workflow(broken, meta)
                self.assertEqual(1, result.returncode, result.stdout + result.stderr)
                self.assertIn(diagnostic, result.stderr)
        # A plausible complete response followed by an API failure must not pass.
        result = self.run_workflow(pages, metadata, api_exit=7)
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("API request failed", result.stderr)
        self.assertNotIn("verified", result.stdout)

    def test_workflow_recovers_advanced_base_without_changing_event_or_head(self):
        pages, metadata = self.workflow_inputs()
        head = metadata["head"]["sha"]
        self.git("checkout", "-qb", "advanced-base", self.base)
        (self.root / "README.md").write_text("base branch advances independently\n")
        advanced = self.commit("advance base")
        self.git("checkout", "--detach", head)
        live = copy.deepcopy(metadata)
        live["base"]["sha"] = advanced
        # Before the fix this exact event B0 / live B1 fixture is refused.
        with self.assertRaisesRegex(history.HistoryError, "pinned event commits"):
            history.api_coverage(json.dumps(pages), json.dumps(live), self.base, head)
        result = self.run_workflow(pages, live, samples=[metadata, live, live, live])
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        snapshot = json.loads((self.workflow_api / "pr-metadata.json").read_text())
        self.assertEqual(snapshot["attempt"], 2)
        self.assertEqual(snapshot["event"]["base"]["sha"], self.base)
        self.assertEqual(snapshot["before"]["base"]["sha"], advanced)
        self.assertEqual(snapshot["before"]["head"]["sha"], head)
        outcomes = json.loads((self.workflow_api / "pr-validation.json").read_text())
        self.assertEqual(outcomes["event"]["base"], self.base)
        self.assertEqual(outcomes["effective"]["base"], advanced)
        self.assertEqual(outcomes["event"]["merge_base"], self.base)
        self.assertEqual(outcomes["effective"]["merge_base"], self.base)

    def test_workflow_original_range_cannot_be_hidden_by_effective_base(self):
        self.write("0001_old.sql")
        self.commit("birth")
        self.rename("0001_old.sql", "0002_final.sql", edit=True)
        advanced = self.git("rev-parse", "HEAD")
        entries = self.entries()  # Deliberately omit the chronological edge.
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        head = self.commit("missing source history")
        files = [[{"filename": "migrations/manifest.toml", "status": "modified"}]]
        live = {"base": {"sha": advanced}, "head": {"sha": head}, "changed_files": 1}
        coverage = history.api_coverage(json.dumps(files), json.dumps(live), advanced, head)
        history.verify(self.root, advanced, head, manifest(entries), coverage)
        result = self.run_workflow(files, live)
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("lacks source_rename_history", result.stderr)
        outcomes = json.loads((self.workflow_api / "pr-validation.json").read_text())
        self.assertEqual(outcomes["event"]["outcome"], "failed")
        self.assertEqual(outcomes["effective"]["outcome"], "not_run")
        self.assertIn("lacks source_rename_history", outcomes["diagnostic"])
        self.assertIn('"outcome": "not_run"', result.stdout)

    def test_workflow_effective_merge_base_manifest_is_independent(self):
        self.write("0001_final.sql")
        (self.root / "migrations/manifest.toml").write_text(manifest(self.entries()))
        self.base = self.commit("original baseline")
        entries = self.entries()
        entries[0]["renamed_from"] = [{"file": "0000_historical.sql", "sha256": entries[0]["sha256"]}]
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        advanced = self.commit("effective baseline with inherited applied alias")
        entries[0]["renamed_from"] = []
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        head = self.commit("remove effective baseline alias")
        history.verify(self.root, self.base, head, manifest(entries))
        files = [[{"filename": "migrations/manifest.toml", "status": "modified"}]]
        live = {"base": {"sha": advanced}, "head": {"sha": head}, "changed_files": 1}
        result = self.run_workflow(files, live)
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("append-only against base", result.stderr)
        report = self.workflow_api / "pr-validation.json"
        outcomes = json.loads(report.read_text())
        self.assertEqual(outcomes["event"]["outcome"], "verified")
        self.assertEqual(outcomes["effective"]["outcome"], "failed")
        self.assertEqual(outcomes["event"]["base"], self.base)
        self.assertEqual(outcomes["effective"]["base"], advanced)
        self.assertIn("append-only against base", outcomes["diagnostic"])
        self.assertLessEqual(len(outcomes["diagnostic"]), 1024)
        self.assertLess(report.stat().st_size, 16384)
        self.assertIn('"outcome": "failed"', result.stdout)
        self.assertNotIn("pinned Git source lineage and applied aliases verified", result.stdout)

    def test_workflow_bracket_exhaustion_and_observed_head_movement_fail(self):
        pages, metadata = self.workflow_inputs()
        changed = copy.deepcopy(metadata)
        changed["changed_files"] += 1
        result = self.run_workflow(pages, metadata, samples=[metadata, changed] * 3)
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("all bounded attempts", result.stderr)
        moved = copy.deepcopy(metadata)
        moved["head"]["sha"] = self.base
        result = self.run_workflow(pages, metadata, samples=[metadata, moved])
        self.assertEqual(1, result.returncode, result.stdout + result.stderr)
        self.assertIn("pinned event commits", result.stderr)

    def test_workflow_both_launchers_isolate_imports_and_refuse_missing_code(self):
        pages, metadata = self.workflow_inputs()
        result = self.run_workflow(pages, metadata, poison=True)
        self.assertEqual(0, result.returncode, result.stdout + result.stderr)
        result = self.run_workflow(pages, metadata, poison=True, missing_code=True)
        self.assertNotEqual(0, result.returncode)
        self.assertIn("ModuleNotFoundError", result.stderr)
        self.assertNotIn("PR-controlled Python must not load", result.stderr)

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

    def workflow_checker(self, entries, *, poison=False, files=None, metadata=None,
                         missing=None, truncated=False, missing_code=False,
                         missing_snapshot=False):
        """Run the real isolated workflow launcher against checker-free PR data."""
        scripts = pathlib.Path(__file__).resolve().parent
        workflow = (scripts.parent / "workflows/migrations-staging.yml").read_text()
        launcher = re.search(r"<<'PY'\n(.*?)^          PY$", workflow, re.M | re.S)
        self.assertIsNotNone(launcher, "workflow must isolate trusted imports from old PR data")
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        head = self.commit("record source metadata")
        self.assertFalse((self.root / ".github/scripts/migrations_rename_check.py").exists())
        if poison:
            # These must remain inert even when the data directory is cwd and
            # PYTHONPATH. No attacker-provided code is executed by the fixture.
            bad = "raise RuntimeError('PR-controlled Python must not load')\n"
            (self.root / ".github/scripts").mkdir(parents=True)
            for directory in (self.root, self.root / ".github/scripts"):
                for name in ("migrations_rename_check.py", "migration_history.py",
                             "migration_snapshot.py", "json.py", "sitecustomize.py"):
                    (directory / name).write_text(bad)
        if files is None:
            files = [[{"filename": "migrations/manifest.toml", "status": "modified"}] + [
                {"filename": "migrations/" + e["file"], "status": "added"} for e in entries]]
        if metadata is None:
            metadata = {"base": {"sha": self.base}, "head": {"sha": head},
                        "changed_files": sum(len(page) for page in files)}
        # Evidence is outside the untrusted checkout, like RUNNER_TEMP in CI.
        with tempfile.TemporaryDirectory() as tmp:
            evidence = pathlib.Path(tmp)
            (evidence / "files.json").write_text(json.dumps(files))
            (evidence / "metadata.json").write_text(json.dumps(metadata))
            if missing:
                (evidence / missing).unlink()
            if truncated:
                (evidence / "files.json").write_text("[[")
            if missing_code:
                scripts = evidence / "absent-trusted-scripts"
            if missing_snapshot:
                # Trusted closure lacking only the snapshot dependency.
                scripts = evidence / "partial-trusted-scripts"
                scripts.mkdir()
                real = pathlib.Path(__file__).resolve().parent
                for name in ("migration_history.py", "migrations_rename_check.py"):
                    shutil.copyfile(real / name, scripts / name)
            args = [sys.executable, "-I", "-", str(scripts), "--root", str(self.root),
                    "--base-sha", self.base, "--head-sha", head,
                    "--files-file", str(evidence / "files.json"),
                    "--metadata-file", str(evidence / "metadata.json")]
            return subprocess.run(args, input=textwrap.dedent(launcher.group(1)),
                                  cwd=self.root, env=dict(os.environ, PYTHONPATH=str(self.root)),
                                  capture_output=True, text=True, timeout=30)

    def test_workflow_trusted_checker_is_available_before_checker_exists(self):
        entries = self.chain()
        result = self.workflow_checker(entries)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("pinned Git source lineage", result.stdout)

    def test_workflow_old_head_cannot_replace_executable_or_imports(self):
        result = self.workflow_checker(self.chain(), poison=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_workflow_trusted_checker_still_refuses_missing_lineage(self):
        entries = self.chain()
        entries[0]["source_rename_history"] = []
        result = self.workflow_checker(entries)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("lacks source_rename_history", result.stderr)

    def test_workflow_trusted_checker_refuses_forged_lineage(self):
        entries = self.chain()
        entries[0]["source_rename_history"][0]["commit"] = "0" * 40
        result = self.workflow_checker(entries)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("forged, missing, unreachable", result.stderr)

    def test_workflow_trusted_checker_refuses_invalid_lineage(self):
        entries = self.chain()
        entries[0]["source_rename_history"][0]["old_path"] = "migrations/../invalid.sql"
        result = self.workflow_checker(entries)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("source rename paths must be distinct flat migration paths", result.stderr)

    def test_workflow_missing_metadata_fails(self):
        self.write("0001_new.sql")
        result = self.workflow_checker(self.entries(), missing="metadata.json")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("cannot read history input", result.stderr)

    def test_workflow_truncated_input_fails(self):
        self.write("0001_new.sql")
        result = self.workflow_checker(self.entries(), truncated=True)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("invalid API JSON", result.stderr)

    def test_workflow_incomplete_api_coverage_fails(self):
        self.write("0001_new.sql")
        result = self.workflow_checker(self.entries(), files=[[
            {"filename": "migrations/manifest.toml", "status": "modified"}]])
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("coverage differs", result.stderr)

    def test_workflow_missing_trusted_code_never_falls_back_to_pr(self):
        self.write("0001_new.sql")
        result = self.workflow_checker(self.entries(), poison=True, missing_code=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ModuleNotFoundError", result.stderr)
        self.assertNotIn("PR-controlled Python must not load", result.stderr)

    def test_workflow_missing_trusted_snapshot_never_falls_back_to_pr(self):
        self.write("0001_new.sql")
        result = self.workflow_checker(self.entries(), poison=True, missing_snapshot=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ModuleNotFoundError", result.stderr)
        self.assertIn("migration_snapshot", result.stderr)
        self.assertNotIn("PR-controlled Python must not load", result.stderr)

    def legacy_cli(self, entries, **overrides):
        """Legacy caller contract: no --event-file, minimal evidence files."""
        (self.root / "migrations/manifest.toml").write_text(manifest(entries))
        head = self.commit("record source metadata")
        files = [{"filename": "migrations/manifest.toml", "status": "modified"}] + [
            {"filename": "migrations/" + e["file"], "status": "added"} for e in entries]
        metadata = {"base": {"sha": self.base}, "head": {"sha": head}, "changed_files": len(files)}
        metadata.update(overrides)
        (self.root / "files.json").write_text(json.dumps([files]))
        (self.root / "metadata.json").write_text(json.dumps(metadata))
        args = ["--root", str(self.root), "--base-sha", self.base, "--head-sha", head,
                "--files-file", str(self.root / "files.json"),
                "--metadata-file", str(self.root / "metadata.json")]
        return args, head

    def test_legacy_caller_contract_without_event_file(self):
        entries = self.chain()
        args, _ = self.legacy_cli(entries)
        self.assertNotIn("--event-file", args)
        output = self.root / "edges.json"
        validation = self.root / "validation.json"
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(migrations_rename_check.main(args + ["--output", str(output)]), 0)
        self.assertEqual("migrations-staging: pinned Git source lineage and applied aliases verified\n",
                         out.getvalue())
        self.assertNotIn("dual history", out.getvalue())
        self.assertFalse(validation.exists())
        edges = json.loads(output.read_text())
        self.assertEqual(json.loads(json.dumps(self.verify(entries))), edges)

    def test_legacy_rejects_moved_base_as_before(self):
        entries = self.chain()
        args, head = self.legacy_cli(entries, base={"sha": "0" * 40})
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.assertEqual(migrations_rename_check.main(args), 1)
        self.assertIn("API metadata does not match pinned event commits", err.getvalue())


class WorkflowBoundary(unittest.TestCase):
    def test_separate_immutable_code_and_data_pins(self):
        workflow = (pathlib.Path(__file__).resolve().parents[1] /
                    "workflows/migrations-staging.yml").read_text()
        code, data = workflow.split("      - name: Check out pinned PR evidence")
        self.assertIn("ref: ${{ github.event.pull_request.base.sha }}", code)
        self.assertIn("path: trusted-checker", code)
        self.assertIn("ref: ${{ github.event.pull_request.head.sha }}", data)
        self.assertIn("path: pr-data", data)
        self.assertIn("fetch-depth: 0", data)
        self.assertEqual(workflow.count("persist-credentials: false"), 2)
        collect = data.split("      - name: Collect pinned PR API evidence")[1].split(
            "      - name: Check migration rename history")[0]
        self.assertIn("working-directory: ${{ runner.temp }}", collect)
        self.assertIn("python3 -I -c", collect)
        self.assertIn('"$GITHUB_WORKSPACE/trusted-checker/.github/scripts"', collect)
        self.assertIn('--event-file "$GITHUB_EVENT_PATH"', collect)
        self.assertIn("BASE_SHA: ${{ github.event.pull_request.base.sha }}", collect)
        self.assertIn("HEAD_SHA: ${{ github.event.pull_request.head.sha }}", collect)
        self.assertIn("GIT_CONFIG_GLOBAL: /dev/null", collect)
        check = workflow.split("      - name: Check migration rename history")[1]
        self.assertIn("working-directory: ${{ runner.temp }}", check)
        self.assertIn('python3 -I - "$GITHUB_WORKSPACE/trusted-checker/.github/scripts"', check)
        self.assertIn('--root "$GITHUB_WORKSPACE/pr-data"', check)
        self.assertIn("GIT_CONFIG_GLOBAL: /dev/null", check)
        self.assertIn("GIT_CONFIG_KEY_0: core.hooksPath", check)
        self.assertIn("GIT_CONFIG_KEY_1: core.fsmonitor", check)
        self.assertNotIn("GH_TOKEN", check)
        self.assertNotIn("secrets.", check)
        self.assertNotIn("pull_request_target", workflow)
        self.assertIn("    name: migrations-staging", workflow)
        self.assertIn("  contents: read", workflow)
        self.assertIn("  pull-requests: read", workflow)


class FreshImportInputs(unittest.TestCase):
    def setUp(self):
        self.source = {
            "file": "0042_final.sql", "target": "app",
            "sha256": hashlib.sha256(BODY).hexdigest(),
            "renamed_from": [{"file": "0041_old.sql", "sha256": hashlib.sha256(BODY).hexdigest()}],
            "source_rename_history": [{
                "parent": "1" * 40, "commit": "2" * 40,
                "old_path": "migrations/0041_old.sql", "new_path": "migrations/0042_final.sql",
                "old_sha256": hashlib.sha256(BODY).hexdigest(),
                "new_sha256": hashlib.sha256(BODY).hexdigest(),
            }],
        }

    def check(self, candidate, baseline=None, sql=BODY):
        history.validate_fresh_import(self.source, candidate, baseline or [], sql)

    def test_exact_entry_and_bytes_match(self):
        self.check(copy.deepcopy(self.source))

    def test_changed_or_missing_metadata_fails(self):
        for key, value in (("file", "0043_other.sql"), ("target", "mirror"),
                           ("sha256", "0" * 64), ("renamed_from", []),
                           ("source_rename_history", []), ("extra", True)):
            with self.subTest(key=key), self.assertRaises(history.HistoryError):
                candidate = copy.deepcopy(self.source)
                candidate[key] = value
                self.check(candidate)

    def test_baseline_current_alias_and_history_collisions_fail(self):
        for name in ("0042_final.sql", "0041_old.sql"):
            with self.subTest(name=name), self.assertRaises(history.HistoryError):
                self.check(copy.deepcopy(self.source), [{
                    "file": name, "target": "app", "sha256": "0" * 64,
                }])
        baseline = [{"file": "0040_other.sql", "target": "app", "sha256": "0" * 64,
                     "renamed_from": [{"file": "0042_final.sql", "sha256": "0" * 64}]}]
        with self.assertRaises(history.HistoryError):
            self.check(copy.deepcopy(self.source), baseline)

    def test_baseline_bounds_precede_copy_and_alias_traversal(self):
        class NoCopy(list):
            def __add__(self, other):
                raise AssertionError("oversized baseline was copied")

        with mock.patch.object(history, "MAX_FILES", 2):
            with self.assertRaisesRegex(history.HistoryError, "entry bound"):
                self.check(copy.deepcopy(self.source), NoCopy([{}, {}]))

        class NoIteration(list):
            def __iter__(self):
                raise AssertionError("oversized aliases were traversed")

        baseline = [{"file": "0040_other.sql", "target": "app", "sha256": "0" * 64,
                     "renamed_from": NoIteration([None] * 1024)}]
        with mock.patch.object(history, "MAX_INPUT", 1024):
            with self.assertRaisesRegex(history.HistoryError, "metadata bound"):
                self.check(copy.deepcopy(self.source), baseline)

        baseline[0]["renamed_from"] = [{"file": "0039_old.sql", "sha256": "0" * 64}]
        self.check(copy.deepcopy(self.source), baseline)
        baseline[0]["renamed_from"][0]["file"] = self.source["file"]
        with self.assertRaises(history.HistoryError):
            self.check(copy.deepcopy(self.source), baseline)

    def test_unicode_metadata_requires_scalar_values_and_keys(self):
        source = copy.deepcopy(self.source)
        source["extra"] = {"\U0001f600": "\U0001f600"}
        history.validate_fresh_import(source, copy.deepcopy(source), [], BODY)
        for invalid in (chr(0xD83D) + chr(0xDE00), chr(0xD83D), chr(0xDE00)):
            for as_key in (False, True):
                candidate = copy.deepcopy(source)
                candidate["extra"] = {invalid: "ok"} if as_key else {"value": invalid}
                with self.subTest(invalid=ascii(invalid), as_key=as_key):
                    with self.assertRaisesRegex(history.HistoryError, "non-scalar Unicode"):
                        history.validate_fresh_import(source, candidate, [], BODY)

    def test_bad_sql_and_oversized_metadata_fail(self):
        for blob in (b"different", bytearray(BODY), b"x" * (history.MAX_BLOB + 1)):
            with self.subTest(size=len(blob)), self.assertRaises(history.HistoryError):
                self.check(copy.deepcopy(self.source), sql=blob)
        source = copy.deepcopy(self.source)
        source["extra"] = "x" * (history.MAX_INPUT + 1)
        with self.assertRaises(history.HistoryError):
            history.validate_fresh_import(source, copy.deepcopy(source), [], BODY)

    def test_metadata_limit_precedes_full_serialization(self):
        candidate = copy.deepcopy(self.source)
        candidate["extra"] = "x" * 2000
        dumps = json.dumps
        serialized = []

        def record(value, *args, **kwargs):
            serialized.append(value)
            return dumps(value, *args, **kwargs)

        with mock.patch.object(history, "MAX_INPUT", 1024), mock.patch.object(
                history.json, "dumps", side_effect=record):
            with self.assertRaisesRegex(history.HistoryError, "bound exceeded"):
                self.check(candidate)
        self.assertFalse(any(value is candidate for value in serialized))
        self.assertFalse(any(value is candidate["extra"] for value in serialized))

    def test_near_limit_metadata_match_and_mismatch(self):
        source = copy.deepcopy(self.source)
        source["extra"] = ""
        base_size = len(json.dumps(source, sort_keys=True, separators=(",", ":")))
        source["extra"] = "x" * (1024 - base_size)
        candidate = copy.deepcopy(source)
        with mock.patch.object(history, "MAX_INPUT", 1024):
            history.validate_fresh_import(source, candidate, [], BODY)
            candidate["extra"] = "y" + candidate["extra"][1:]
            with self.assertRaisesRegex(history.HistoryError, "differs"):
                history.validate_fresh_import(source, candidate, [], BODY)
            candidate["extra"] += "x"
            with self.assertRaisesRegex(history.HistoryError, "bound exceeded"):
                history.validate_fresh_import(source, candidate, [], BODY)

    def test_nested_aggregate_and_escaped_metadata_bounds(self):
        for extra in (["x" * 300] * 4, "\\n" * 300):
            candidate = copy.deepcopy(self.source)
            candidate["extra"] = extra
            with self.subTest(extra=type(extra)), mock.patch.object(history, "MAX_INPUT", 1024):
                with self.assertRaisesRegex(history.HistoryError, "bound exceeded"):
                    self.check(candidate)
        candidate = copy.deepcopy(self.source)
        candidate["extra"] = []
        candidate["extra"].append(candidate["extra"])
        with self.assertRaisesRegex(history.HistoryError, "cycle"):
            self.check(candidate)


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
