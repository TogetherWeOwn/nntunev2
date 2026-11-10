#!/usr/bin/env python3
"""Bounded PR API snapshot collection: strict projection, brackets and budgets."""
import contextlib
import copy
import io
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import migration_history as history
import migration_snapshot as snapshot
import test_migrations_rename_check as rename_fixture

BODY = rename_fixture.BODY


def sha(number):
    return format(number, "040x")


def meta(*, base=sha(1), head=sha(2), count=0, number=7, base_ref="main",
         head_ref="topic", base_repo=(1, "o/r"), head_repo=(2, "f/r")):
    return {"number": number, "changed_files": count, "title": "ignored",
            "base": {"sha": base, "ref": base_ref, "label": "x",
                     "repo": {"id": base_repo[0], "full_name": base_repo[1], "private": False}},
            "head": {"sha": head, "ref": head_ref,
                     "repo": {"id": head_repo[0], "full_name": head_repo[1]}}}


def files(count, start=0, prefix="docs"):
    return [{"filename": f"{prefix}/f{i}.md", "status": "modified", "sha": "x", "patch": "@@"}
            for i in range(start, start + count)]


class Script:
    """Ordered fake for snapshot.request; exceptions in the script are raised."""

    def __init__(self, *items):
        self.items, self.paths = list(items), []

    def __call__(self, path, budget):
        self.paths.append(path)
        item = self.items.pop(0)
        if isinstance(item, Exception):
            raise item
        return copy.deepcopy(item)


class GitCase(unittest.TestCase):
    """Real Git topology: B0 (fixture base) with child B1 on main and H on a side branch.

    Reuses the real fixture's setup/helpers without inheriting its test methods.
    """
    git = rename_fixture.GitFixture.git
    commit = rename_fixture.GitFixture.commit
    write = rename_fixture.GitFixture.write

    def setUp(self):
        rename_fixture.GitFixture.setUp(self)
        self.git("checkout", "-q", "-b", "head")
        self.write("0002_real.sql")
        self.head = self.commit("head adds migration")
        self.git("checkout", "-q", "main")
        (self.root / "main.txt").write_text("moved\n")
        self.moved = self.commit("main moves")
        self.git("checkout", "-q", "--orphan", "other")
        self.git("commit", "-q", "--allow-empty", "-m", "unrelated")
        self.unrelated = self.git("rev-parse", "HEAD")
        self.git("checkout", "-q", "-f", "main")

    def expected(self, **kw):
        kw.setdefault("base", self.base)
        kw.setdefault("head", self.head)
        return snapshot.project_metadata(meta(**kw))

    def collect(self, *items, expected=None, budget=None):
        script = Script(*items)
        budget = budget or history.EvidenceBudget()
        with mock.patch.object(snapshot, "request", side_effect=script):
            try:
                return snapshot.collect(self.root, expected or self.expected(), budget), script
            except history.HistoryError as exc:
                exc.script = script
                raise

    def fails(self, pattern, *items, **kw):
        with self.assertRaisesRegex(history.HistoryError, pattern) as ctx:
            self.collect(*items, **kw)
        return ctx.exception.script

    def one_page(self, records, count=None, **kw):
        count = len(records) if count is None else count
        m = meta(base=self.base, head=self.head, count=count, **kw)
        return [m, records, m]


class ProjectionAndDecode(unittest.TestCase):
    def test_projection_keeps_only_pinned_identity(self):
        self.assertEqual(snapshot.project_metadata(meta(count=3)), {
            "number": 7, "changed_files": 3,
            "base": {"sha": sha(1), "ref": "main", "repo": {"id": 1, "full_name": "o/r"}},
            "head": {"sha": sha(2), "ref": "topic", "repo": {"id": 2, "full_name": "f/r"}}})

    def test_projection_rejects_malformed_identity(self):
        def mutate(fn):
            data = meta()
            fn(data)
            return data
        cases = {
            "missing head": mutate(lambda d: d.pop("head")),
            "missing count": mutate(lambda d: d.pop("changed_files")),
            "short sha": mutate(lambda d: d["base"].update(sha="abc")),
            "uppercase sha": mutate(lambda d: d["head"].update(sha=sha(255).upper().replace("X", "F"))),
            "empty ref": mutate(lambda d: d["base"].update(ref="")),
            "non-string ref": mutate(lambda d: d["head"].update(ref=None)),
            "bool repo id": mutate(lambda d: d["base"]["repo"].update(id=True)),
            "zero repo id": mutate(lambda d: d["head"]["repo"].update(id=0)),
            "string repo id": mutate(lambda d: d["head"]["repo"].update(id="2")),
            "bad full name": mutate(lambda d: d["base"]["repo"].update(full_name="a/b/c")),
            "zero number": mutate(lambda d: d.update(number=0)),
            "bool number": mutate(lambda d: d.update(number=True)),
            "count at bound": mutate(lambda d: d.update(changed_files=history.MAX_FILES)),
            "negative count": mutate(lambda d: d.update(changed_files=-1)),
            "bool count": mutate(lambda d: d.update(changed_files=False)),
            "float count": mutate(lambda d: d.update(changed_files=1.0)),
            "not an object": [],
            "null": None,
        }
        for name, data in cases.items():
            with self.subTest(name), self.assertRaises(history.HistoryError):
                snapshot.project_metadata(data)

    def test_count_just_below_bound_is_accepted(self):
        data = meta(count=history.MAX_FILES - 1)
        self.assertEqual(snapshot.project_metadata(data)["changed_files"], 2999)

    def test_decode_rejects_duplicate_and_invalid_json(self):
        for text in ('{"a":1,"a":2}', '{"a":{"b":1,"b":1}}', '[{"x":1,"x":1}]', "{", "",
                     "[" * 100000):
            with self.subTest(text[:20]), self.assertRaisesRegex(history.HistoryError, "API JSON"):
                snapshot.decode(text)
        self.assertEqual(snapshot.decode('{"a":[1,{"b":2}]}'), {"a": [1, {"b": 2}]})

    def test_check_identity_hard_failures(self):
        expected = snapshot.project_metadata(meta())
        snapshot.check_identity(snapshot.project_metadata(meta(base=sha(9))), expected)
        for kw, pattern in (
                ({"number": 8}, "PR identity"),
                ({"head": sha(9)}, "head identity"),
                ({"head_ref": "other"}, "head identity"),
                ({"head_repo": (3, "f/r")}, "head identity"),
                ({"base_ref": "release"}, "retargeted"),
                ({"base_repo": (9, "o/r")}, "retargeted"),
                ({"base_repo": (1, "o/renamed")}, "retargeted")):
            with self.subTest(kw), self.assertRaisesRegex(history.HistoryError, pattern):
                snapshot.check_identity(snapshot.project_metadata(meta(**kw)), expected)


class EventMetadata(unittest.TestCase):
    def event(self, **overrides):
        value = {"number": 7, "repository": {"id": 1, "full_name": "o/r"},
                 "pull_request": meta(base=sha(1), head=sha(2))}
        value.update(overrides)
        handle = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
        self.addCleanup(os.unlink, handle.name)
        with handle:
            json.dump(value, handle)
        return handle.name

    def test_event_matches_pinned_commits_and_repository(self):
        self.assertEqual(snapshot.event_metadata(self.event(), sha(1), sha(2))["number"], 7)

    def test_event_mismatches_fail(self):
        cases = (
            (self.event(), sha(3), sha(2)),
            (self.event(), sha(1), sha(3)),
            (self.event(number=8), sha(1), sha(2)),
            (self.event(repository={"id": 5, "full_name": "o/r"}), sha(1), sha(2)),
            (self.event(repository={"id": 1, "full_name": "x/y"}), sha(1), sha(2)),
            (self.event(repository=None), sha(1), sha(2)),
        )
        for path, base, head in cases:
            with self.subTest(path=path, base=base, head=head), \
                    self.assertRaises(history.HistoryError):
                snapshot.event_metadata(path, base, head)

    def test_missing_pull_request_fails(self):
        path = self.event()
        data = json.loads(pathlib.Path(path).read_text())
        del data["pull_request"]
        pathlib.Path(path).write_text(json.dumps(data))
        with self.assertRaisesRegex(history.HistoryError, "missing pinned event identity"):
            snapshot.event_metadata(path, sha(1), sha(2))


class RawRequest(unittest.TestCase):
    def run_request(self, code, budget=None):
        real = subprocess.Popen

        def popen(argv, **kw):
            self.assertEqual(argv, ["gh", "api", "repos/o/r"])
            return real([sys.executable, "-I", "-c", code], **kw)
        with mock.patch.object(subprocess, "Popen", popen):
            return snapshot.request("repos/o/r", budget or history.EvidenceBudget())

    def test_successful_response_is_decoded_and_counted(self):
        budget = history.EvidenceBudget()
        self.assertEqual(self.run_request("print('{\"a\": [1]}')", budget), {"a": [1]})
        self.assertGreater(budget.total_output, 0)

    def test_nonzero_exit_is_not_evidence(self):
        with self.assertRaisesRegex(history.HistoryError, "no evidence accepted"):
            self.run_request("import sys; print('[]'); sys.exit(1)")

    def test_raw_duplicate_keys_and_invalid_json_fail(self):
        with self.assertRaisesRegex(history.HistoryError, "duplicate API JSON key"):
            self.run_request("print('{\"a\":1,\"a\":2}')")
        with self.assertRaisesRegex(history.HistoryError, "invalid API JSON"):
            self.run_request("print('[')")

    def test_invalid_utf8_fails_closed(self):
        with self.assertRaisesRegex(history.HistoryError, "API request failed"):
            self.run_request("import sys; sys.stdout.buffer.write(b'[\\xff]')")

    def test_per_response_byte_bound(self):
        code = f"import sys; sys.stdout.buffer.write(b'0' * {history.MAX_OUTPUT + 1})"
        with self.assertRaisesRegex(history.HistoryError, "byte bound"):
            self.run_request(code)

    def test_response_at_byte_bound_is_not_oversize(self):
        # A valid JSON string of exactly MAX_OUTPUT bytes (quotes included).
        code = f"import sys; sys.stdout.buffer.write(b'\"' + b'a' * {history.MAX_OUTPUT - 2} + b'\"')"
        self.assertEqual(len(self.run_request(code)), history.MAX_OUTPUT - 2)

    def test_stalled_response_times_out(self):
        code = "import sys, time; sys.stdout.write('['); sys.stdout.flush(); time.sleep(30)"
        with mock.patch.object(snapshot, "REQUEST_TIMEOUT", 0.3), \
                self.assertRaisesRegex(history.HistoryError, "timed out"):
            self.run_request(code)

    def test_cumulative_output_bound_applies_to_api_bytes(self):
        budget = history.EvidenceBudget(total_output=history.MAX_TOTAL_OUTPUT)
        with self.assertRaisesRegex(history.HistoryError, "shared evidence output bound"):
            self.run_request("print('[]')", budget)

    def test_expired_shared_deadline_never_starts_a_request(self):
        budget = history.EvidenceBudget(deadline=0)
        with mock.patch.object(subprocess, "Popen") as popen, \
                self.assertRaisesRegex(history.HistoryError, "deadline exceeded"):
            snapshot.request("repos/o/r", budget)
        popen.assert_not_called()


class Collect(GitCase):
    def test_successful_collection_keeps_every_page_record_unfiltered(self):
        records = files(100) + [{"filename": "migrations/0002_real.sql", "status": "added"}]
        m = meta(base=self.base, head=self.head, count=101)
        first, second = records[:100], records[100:]
        (pages, evidence), script = self.collect(m, first, second, m)
        self.assertEqual(script.paths, [
            "repos/o/r/pulls/7", "repos/o/r/pulls/7/files?per_page=100&page=1",
            "repos/o/r/pulls/7/files?per_page=100&page=2", "repos/o/r/pulls/7"])
        # Non-migration records survive; only the three evidence keys are projected.
        self.assertEqual(pages[0][0], {"filename": "docs/f0.md", "status": "modified"})
        self.assertEqual(sum(len(p) for p in pages), 101)
        self.assertEqual(evidence["attempt"], 1)
        self.assertEqual(evidence["before"], evidence["after"])
        self.assertEqual(evidence["event"], self.expected())
        self.assertIn("deadline", evidence)
        self.assertGreaterEqual(evidence["total_output"], 0)

    def test_previous_filename_is_retained_for_renames(self):
        record = {"filename": "docs/new.md", "status": "renamed", "previous_filename": "docs/old.md",
                  "extra": 1}
        (pages, _), _ = self.collect(*self.one_page([record]))
        self.assertEqual(pages, [[{"filename": "docs/new.md", "status": "renamed",
                                   "previous_filename": "docs/old.md"}]])

    def test_zero_files_and_exact_full_page_followed_by_empty_page(self):
        (pages, _), _ = self.collect(*self.one_page([]))
        self.assertEqual(pages, [[]])
        m = meta(base=self.base, head=self.head, count=100)
        (pages, _), script = self.collect(m, files(100), [], m)
        self.assertEqual([len(p) for p in pages], [100, 0])
        self.assertEqual(len(script.paths), 4)

    def test_base_movement_within_descendant_bound_retries_then_succeeds(self):
        before = meta(base=self.base, head=self.head, count=1)
        moved = meta(base=self.moved, head=self.head, count=1)
        (pages, evidence), script = self.collect(
            before, files(1), moved,
            moved, files(1), moved)
        self.assertEqual(evidence["attempt"], 2)
        self.assertEqual(evidence["before"]["base"]["sha"], self.moved)
        self.assertEqual(len(script.paths), 6)

    def test_count_change_between_brackets_retries(self):
        a = meta(base=self.base, head=self.head, count=1)
        b = meta(base=self.base, head=self.head, count=2)
        (_, evidence), _ = self.collect(a, files(1), b, b, files(2), b)
        self.assertEqual(evidence["attempt"], 2)

    def test_count_change_before_files_fetch_retries_only_valid_closing_count(self):
        for opening_count, closing_count in ((1, 2), (2, 1)):
            with self.subTest(opening=opening_count, closing=closing_count):
                before = meta(base=self.base, head=self.head, count=opening_count)
                after = meta(base=self.moved, head=self.head, count=closing_count)
                records = files(closing_count)
                (pages, evidence), script = self.collect(
                    before, records, after, after, records, after)
                self.assertEqual(evidence["attempt"], 2)
                self.assertEqual(evidence["before"]["base"]["sha"], self.moved)
                self.assertEqual(sum(map(len, pages)), closing_count)
                self.assertEqual(script.items, [])

        before = meta(base=self.base, head=self.head, count=1)
        after = meta(base=self.moved, head=self.head, count=2)
        for records, pattern in (
                (files(3), "truncated or duplicated"),
                (files(1) + files(1), "invalid or duplicate"),
                ([{"filename": "docs/a.md", "status": "bogus"}] + files(1),
                 "invalid or duplicate"),
                ([{"filename": "migrations/0002_Bad-name.sql", "status": "added"}]
                 + files(1), "invalid migration path")):
            with self.subTest(pattern=pattern, records=records):
                script = self.fails(pattern, before, records, after,
                                    after, files(2), after)
                self.assertEqual(len(script.paths), 3)

    def test_observed_rewinds_and_sibling_transitions_are_not_retried_away(self):
        (self.root / "next.txt").write_text("another advance\n")
        newest = self.commit("second advance")
        self.git("checkout", "-qb", "sibling", self.base)
        (self.root / "sibling.txt").write_text("independent descendant\n")
        sibling = self.commit("sibling advance")
        b1 = meta(base=self.moved, head=self.head, count=1)
        b2 = meta(base=newest, head=self.head, count=1)
        for opening, closing in ((b2, b1),
                                 (b1, meta(base=sibling, head=self.head, count=1))):
            with self.subTest(closing=closing["base"]["sha"]):
                script = self.fails("not a descendant", opening, files(1), closing,
                                    closing, files(1), closing)
                self.assertEqual(len(script.paths), 3)
        # A count mismatch alone can retry, but cannot erase the prior B2 sample.
        changed_count = copy.deepcopy(b2)
        changed_count["changed_files"] = 2
        script = self.fails("not a descendant", b2, files(1), changed_count,
                            b1, files(1), b1)
        self.assertEqual(len(script.paths), 4)

    def test_retry_exhaustion_after_three_attempts(self):
        before = meta(base=self.base, head=self.head, count=1)
        moved = meta(base=self.base, head=self.head, count=2)
        script = self.fails("changed during all bounded attempts",
                            *([before, files(1), moved] * snapshot.MAX_ATTEMPTS))
        self.assertEqual(len(script.paths), 3 * snapshot.MAX_ATTEMPTS)
        self.assertEqual(script.items, [])

    def test_moved_head_is_a_hard_failure_without_retry(self):
        good = meta(base=self.base, head=self.head, count=0)
        moved_head = meta(base=self.base, head=sha(9), count=0)
        for items in ([moved_head], [good, [], moved_head]):
            with self.subTest(len(items)):
                script = self.fails("head identity", *items, *[good, [], good] * 2)
                self.assertLessEqual(len(script.paths), 3)

    def test_repository_ref_and_number_retarget_are_hard_failures(self):
        for kw, pattern in (({"base_repo": (9, "o/r")}, "retargeted"),
                            ({"base_ref": "release"}, "retargeted"),
                            ({"head_repo": (9, "f/r")}, "head identity"),
                            ({"number": 8}, "PR identity")):
            with self.subTest(kw):
                bad = meta(base=self.base, head=self.head, **kw)
                good = meta(base=self.base, head=self.head)
                # Retargeting in the closing bracket must not be retried away.
                script = self.fails(pattern, good, [], bad, *[good, [], good] * 2)
                self.assertEqual(len(script.paths), 3)

    def test_rewound_or_unrelated_effective_base_fails(self):
        expected = self.expected(base=self.moved)
        rewound = meta(base=self.base, head=self.head)
        script = self.fails("not a descendant of event base", rewound, expected=expected)
        self.assertEqual(len(script.paths), 1)
        for target in (self.unrelated, sha(0xabc)):
            with self.subTest(target):
                self.fails(".", meta(base=target, head=self.head))

    def test_descendant_effective_base_is_accepted(self):
        m = meta(base=self.moved, head=self.head)
        (_, evidence), _ = self.collect(m, [], m)
        self.assertEqual(evidence["before"]["base"]["sha"], self.moved)

    def test_api_or_request_failure_propagates(self):
        self.fails("API request failed", history.HistoryError("API request failed"))
        self.fails("API request failed", meta(base=self.base, head=self.head),
                   history.HistoryError("API request failed"))

    def test_pagination_malformed(self):
        m = meta(base=self.base, head=self.head, count=1)
        for name, page in (("object", {"a": 1}), ("non-dict record", ["x"]),
                           ("oversize page", files(101))):
            with self.subTest(name):
                self.fails("malformed or incomplete", m, page)

    def test_pagination_reaching_truncation_bound(self):
        m = meta(base=self.base, head=self.head, count=1)
        pages = [files(100, start=i * 100) for i in range(history.MAX_FILES // 100)]
        script = self.fails("truncation bound", m, *pages)
        self.assertEqual(len(script.paths), 1 + history.MAX_FILES // 100)

    def test_count_cap_and_truncated_or_duplicated_lists(self):
        capped = meta(base=self.base, head=self.head, count=history.MAX_FILES)
        self.fails("count missing or at truncation bound", capped)
        self.fails("truncated or duplicated", *self.one_page(files(1), count=2))
        self.fails("truncated or duplicated", *self.one_page(files(2), count=1))
        m = meta(base=self.base, head=self.head, count=101)
        self.fails("truncated or duplicated", m, files(100), [], m)

    def test_duplicate_filenames_within_and_across_pages(self):
        dup = files(1) + files(1)
        self.fails("invalid or duplicate", *self.one_page(dup))
        m = meta(base=self.base, head=self.head, count=101)
        self.fails("invalid or duplicate", m, files(100), files(1), m)

    def test_invalid_statuses_and_record_shapes(self):
        base = {"filename": "docs/a.md"}
        cases = (
            {**base, "status": "bogus"}, {**base, "status": None}, base,
            {"status": "added"}, {"filename": 1, "status": "added"},
            {**base, "status": "renamed"},
            {**base, "status": "renamed", "previous_filename": "docs/a.md"},
            {**base, "status": "renamed", "previous_filename": 3},
            {**base, "status": "modified", "previous_filename": "docs/b.md"},
        )
        for record in cases:
            with self.subTest(record):
                self.fails("API rename|invalid or duplicate|unexpected API",
                           *self.one_page([record]))
        for status in ("added", "removed", "modified", "renamed", "changed", "copied"):
            record = {**base, "status": status}
            if status == "renamed":
                record["previous_filename"] = "docs/b.md"
            with self.subTest(status):
                self.collect(*self.one_page([record]))

    def test_invalid_migration_paths_and_ambiguous_changes(self):
        for name in ("migrations/0002_Bad-name.sql", "migrations/sub/0002_a.sql",
                     "migrations/2_a.SQL"):
            with self.subTest(name):
                self.fails("invalid migration path",
                           *self.one_page([{"filename": name, "status": "added"}]))
        old, new = "migrations/0003_old.sql", "migrations/0003_new.sql"
        records = [{"filename": new, "status": "renamed", "previous_filename": old},
                   {"filename": old, "status": "added"}]
        self.fails("ambiguous net", *self.one_page(records))

    def test_equal_count_substituted_migration_differs_from_git_net_changes(self):
        sql_sha = hashlib.sha256(BODY).hexdigest()
        text = rename_fixture.manifest([{"file": "0002_real.sql", "target": "app", "sha256": sql_sha}])
        honest = [{"filename": "migrations/0002_real.sql", "status": "added"}]
        forged = [{"filename": "migrations/0002_other.sql", "status": "added"}]
        for records, ok in ((honest, True), (forged, False)):
            with self.subTest(ok=ok):
                # Both bracketed collections succeed with identical counts; only Git decides.
                (pages, evidence), _ = self.collect(*self.one_page(records))
                coverage = history.api_coverage(
                    json.dumps(pages), json.dumps(evidence["before"]), self.base, self.head)
                if ok:
                    history.verify(self.root, self.base, self.head, text, coverage)
                else:
                    with self.assertRaisesRegex(history.HistoryError, "coverage differs"):
                        history.verify(self.root, self.base, self.head, text, coverage)

    def test_expired_shared_deadline_fails_before_any_request(self):
        script = self.fails("deadline exceeded", budget=history.EvidenceBudget(deadline=0))
        self.assertEqual(script.paths, [])

    def test_deadline_expiring_mid_collection_fails_in_git_pass(self):
        budget = history.EvidenceBudget()
        m = meta(base=self.base, head=self.head)

        def expire(path, b):
            b.deadline = 0
            return copy.deepcopy(m)
        with mock.patch.object(snapshot, "request", side_effect=expire), \
                self.assertRaisesRegex(history.HistoryError, "deadline exceeded"):
            snapshot.collect(self.root, self.expected(), budget)

    def test_collection_window_is_bounded_separately(self):
        m = meta(base=self.base, head=self.head)
        with mock.patch.object(snapshot, "COLLECTION_TIMEOUT", 1e-9):
            self.fails("collection timed out|deadline exceeded", m, [], m)

    def test_cumulative_output_bound_covers_git_passes(self):
        budget = history.EvidenceBudget(total_output=history.MAX_TOTAL_OUTPUT)
        m = meta(base=self.base, head=self.head)
        self.fails("output bound exceeded", m, [], m, budget=budget)

    def test_cumulative_output_is_shared_by_api_and_git(self):
        m = meta(base=self.base, head=self.head)
        budget = history.EvidenceBudget()
        (_, evidence), _ = self.collect(m, [], m, budget=budget)
        self.assertEqual(evidence["total_output"], budget.total_output)
        self.assertGreater(budget.total_output, 0)  # Git passes charged the shared budget

    def test_compact_output_bound(self):
        records = [{"filename": "docs/" + "x" * 25000, "status": "added"} for _ in range(100)]
        m = meta(base=self.base, head=self.head, count=100)
        self.fails("input byte bound exceeded", m, records)
        with self.assertRaisesRegex(history.HistoryError, "input byte bound"):
            snapshot.compact("x" * (history.MAX_INPUT + 1))


class SnapshotAndResume(GitCase):
    def evidence(self, count=0, records=()):
        m = meta(base=self.base, head=self.head, count=count)
        (pages, evidence), _ = self.collect(m, list(records), m)
        return json.dumps(pages), json.dumps(evidence), evidence

    def test_snapshot_coverage_round_trip(self):
        files_text, text, _ = self.evidence(1, [{"filename": "migrations/0002_real.sql",
                                                 "status": "added"}])
        effective, (changes, pairs) = snapshot.snapshot_coverage(
            self.root, self.base, self.head, files_text, text, self.expected(),
            history.EvidenceBudget())
        self.assertEqual(effective, self.base)
        self.assertEqual(changes, {"migrations/0002_real.sql": "A"})
        self.assertEqual(pairs, set())

    def test_snapshot_forgery_fails(self):
        files_text, text, evidence = self.evidence()
        for name, mutate in (
                ("bracket differs", lambda e: e["after"].update(changed_files=1)),
                ("event differs", lambda e: e["event"]["base"].update(ref="release")),
                ("attempt zero", lambda e: e.update(attempt=0)),
                ("attempt above bound", lambda e: e.update(attempt=snapshot.MAX_ATTEMPTS + 1)),
                ("attempt bool", lambda e: e.update(attempt=True)),
                ("missing before", lambda e: e.pop("before")),
                ("head differs", lambda e: [e[k]["head"].update(sha=sha(9)) for k in ("before", "after")]),
                ("rewound base", lambda e: [e[k]["base"].update(sha=sha(9)) for k in ("before", "after")])):
            data = copy.deepcopy(evidence)
            mutate(data)
            with self.subTest(name), self.assertRaises(history.HistoryError):
                snapshot.snapshot_coverage(self.root, self.base, self.head, files_text,
                                           json.dumps(data), self.expected(), history.EvidenceBudget())

    def test_snapshot_effective_base_must_descend_from_event_base(self):
        files_text, _, evidence = self.evidence()
        evidence["event"] = self.expected(base=self.moved)
        with self.assertRaisesRegex(history.HistoryError, "not a descendant"):
            snapshot.snapshot_coverage(
                self.root, self.moved, self.head, files_text, json.dumps(evidence),
                self.expected(base=self.moved), history.EvidenceBudget())

    def test_resumed_budget_accepts_live_values(self):
        deadline = time.monotonic() + 100
        budget = snapshot.resumed_budget(json.dumps({"deadline": deadline, "total_output": 5}))
        self.assertEqual((budget.deadline, budget.total_output), (deadline, 5))
        snapshot.resumed_budget(json.dumps({"deadline": deadline, "total_output": 0}))
        snapshot.resumed_budget(json.dumps({"deadline": int(deadline), "total_output": 0}))

    def test_resumed_budget_rejects_invalid_values(self):
        live = time.monotonic() + 100
        cases = {
            "missing": "{}", "list": "[]", "null": "null",
            "no output": json.dumps({"deadline": live}),
            "no deadline": json.dumps({"total_output": 0}),
            "expired": json.dumps({"deadline": time.monotonic() - 1, "total_output": 0}),
            "zero": json.dumps({"deadline": 0, "total_output": 0}),
            "too far": json.dumps({"deadline": time.monotonic() + 3600, "total_output": 0}),
            "string deadline": json.dumps({"deadline": str(live), "total_output": 0}),
            "bool deadline": json.dumps({"deadline": True, "total_output": 0}),
            "null deadline": json.dumps({"deadline": None, "total_output": 0}),
            "nan": '{"deadline": NaN, "total_output": 0}',
            "infinity": '{"deadline": Infinity, "total_output": 0}',
            "negative output": json.dumps({"deadline": live, "total_output": -1}),
            "float output": json.dumps({"deadline": live, "total_output": 1.5}),
            "bool output": json.dumps({"deadline": live, "total_output": True}),
            "string output": json.dumps({"deadline": live, "total_output": "0"}),
            "over total bound": json.dumps({"deadline": live,
                                            "total_output": history.MAX_TOTAL_OUTPUT + 1}),
            "duplicate key": '{"deadline": %r, "deadline": %r, "total_output": 0}' % (live, live),
            "invalid json": "{",
        }
        for name, text in cases.items():
            with self.subTest(name), self.assertRaises(history.HistoryError):
                snapshot.resumed_budget(text)

    def test_resumed_budget_at_total_bound_is_accepted_but_exhausted(self):
        text = json.dumps({"deadline": time.monotonic() + 100,
                           "total_output": history.MAX_TOTAL_OUTPUT})
        budget = snapshot.resumed_budget(text)
        with self.assertRaisesRegex(history.HistoryError, "output bound"):
            budget.consume(1)


class PreparedCLI(unittest.TestCase):
    """Exercise opt-in support directly; the staging workflow stays on its old caller."""
    setUp = rename_fixture.GitFixture.setUp
    git = rename_fixture.GitFixture.git
    commit = rename_fixture.GitFixture.commit
    write = rename_fixture.GitFixture.write
    rename = rename_fixture.GitFixture.rename
    entries = rename_fixture.GitFixture.entries
    prepare = rename_fixture.GitFixture.prepare

    def acquire(self, records, effective, samples=None):
        head = self.git("rev-parse", "HEAD")
        expected = snapshot.project_metadata(meta(base=self.base, head=head, count=len(records)))
        live = copy.deepcopy(expected)
        live["base"]["sha"] = effective
        self.evidence_dir = self.root / "evidence"
        self.evidence_dir.mkdir()
        self.event_file = self.evidence_dir / "event.json"
        self.event_file.write_text(json.dumps({"number": 7, "repository": expected["base"]["repo"],
                                               "pull_request": expected}))
        script = Script(*(samples or [live, records, live]))
        with mock.patch.object(snapshot, "request", side_effect=script), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(snapshot.main([
                "--root", str(self.root), "--base-sha", self.base, "--head-sha", head,
                "--event-file", str(self.event_file), "--output-dir", str(self.evidence_dir)]), 0)
        self.args = ["--root", str(self.root), "--base-sha", self.base, "--head-sha", head,
                     "--event-file", str(self.event_file),
                     "--files-file", str(self.evidence_dir / "pr-files.json"),
                     "--metadata-file", str(self.evidence_dir / "pr-metadata.json"),
                     "--validation-output", str(self.evidence_dir / "validation.json"),
                     "--output", str(self.evidence_dir / "edges.json")]

    def check(self):
        out, err = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            result = rename_fixture.migrations_rename_check.main(self.args)
        return result, out.getvalue(), err.getvalue()

    def valid_evidence(self):
        self.write("0001_final.sql")
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(self.entries()))
        self.commit("add migration")
        self.acquire([{"filename": "migrations/0001_final.sql", "status": "added"},
                      {"filename": "migrations/manifest.toml", "status": "modified"}], self.base)

    def test_prepared_cli_recovers_advanced_base_and_records_both_histories(self):
        self.write("0001_final.sql")
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(self.entries()))
        head = self.commit("add migration")
        self.git("checkout", "-qb", "advanced-base", self.base)
        (self.root / "README.md").write_text("base advances independently\n")
        advanced = self.commit("advance base")
        self.git("checkout", "--detach", head)
        records = [{"filename": "migrations/0001_final.sql", "status": "added"},
                   {"filename": "migrations/manifest.toml", "status": "modified"}]
        before = snapshot.project_metadata(meta(base=self.base, head=head, count=2))
        live = copy.deepcopy(before)
        live["base"]["sha"] = advanced
        with self.assertRaisesRegex(history.HistoryError, "pinned event commits"):
            history.api_coverage(json.dumps([records]), json.dumps(live), self.base, head)
        self.acquire(records, advanced, [before, records, live, live, records, live])
        result, out, err = self.check()
        self.assertEqual(result, 0, out + err)
        outcomes = json.loads((self.evidence_dir / "validation.json").read_text())
        self.assertEqual(outcomes["event"]["base"], self.base)
        self.assertEqual(outcomes["effective"]["base"], advanced)
        for outcome in outcomes.values():
            self.assertEqual(outcome["head"], head)
            self.assertEqual(outcome["merge_base"], self.base)
            self.assertEqual(outcome["outcome"], "verified")
        evidence = json.loads((self.evidence_dir / "pr-metadata.json").read_text())
        self.assertEqual(evidence["attempt"], 2)
        self.assertEqual(evidence["event"]["base"]["sha"], self.base)

    def test_original_range_cannot_be_hidden_by_effective_base(self):
        self.write("0001_old.sql")
        self.commit("birth")
        self.rename("0001_old.sql", "0002_final.sql", edit=True)
        advanced = self.git("rev-parse", "HEAD")
        entries = self.entries()  # Deliberately omit the event-range edge.
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(entries))
        head = self.commit("missing source history")
        records = [{"filename": "migrations/manifest.toml", "status": "modified"}]
        coverage = history.api_coverage(json.dumps([records]), json.dumps({
            "base": {"sha": advanced}, "head": {"sha": head}, "changed_files": 1}), advanced, head)
        history.verify(self.root, advanced, head, rename_fixture.manifest(entries), coverage)
        self.acquire(records, advanced)
        result, out, err = self.check()
        self.assertEqual(result, 1, out + err)
        self.assertIn("lacks source_rename_history", err)
        self.assertFalse((self.evidence_dir / "validation.json").exists())

    def test_effective_merge_base_manifest_is_independent(self):
        self.write("0001_final.sql")
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(self.entries()))
        self.base = self.commit("original baseline")
        entries = self.entries()
        entries[0]["renamed_from"] = [{"file": "0000_historical.sql", "sha256": entries[0]["sha256"]}]
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(entries))
        advanced = self.commit("effective baseline with inherited applied alias")
        entries[0]["renamed_from"] = []
        (self.root / "migrations/manifest.toml").write_text(rename_fixture.manifest(entries))
        head = self.commit("remove effective baseline alias")
        history.verify(self.root, self.base, head, rename_fixture.manifest(entries))
        self.acquire([{"filename": "migrations/manifest.toml", "status": "modified"}], advanced)
        result, out, err = self.check()
        self.assertEqual(result, 1, out + err)
        self.assertIn("append-only against base", err)
        self.assertFalse((self.evidence_dir / "validation.json").exists())

    def test_missing_tampered_and_oversized_evidence_fail_closed(self):
        self.valid_evidence()
        for name in ("event.json", "pr-files.json", "pr-metadata.json"):
            path = self.evidence_dir / name
            original = path.read_bytes()
            for damage in (None, b"{}", b"[" * (history.MAX_INPUT + 1)):
                with self.subTest(file=name, damage=None if damage is None else len(damage)):
                    if damage is None:
                        path.unlink()
                    else:
                        path.write_bytes(damage)
                    try:
                        result, out, err = self.check()
                        self.assertEqual(result, 1, out + err)
                        self.assertFalse((self.evidence_dir / "validation.json").exists())
                        self.assertFalse((self.evidence_dir / "edges.json").exists())
                    finally:
                        path.write_bytes(original)

    def test_shared_budget_is_not_reset_between_original_and_effective_passes(self):
        self.valid_evidence()
        budgets = []
        real_verify = history.verify

        def verify(*args, **kw):
            budget = kw["budget"]
            budgets.append(budget)
            if len(budgets) == 2:
                budget.consume(1)
            result = real_verify(*args, **kw)
            budget.total_output = history.MAX_TOTAL_OUTPUT
            return result

        with mock.patch.object(rename_fixture.migrations_rename_check, "verify", side_effect=verify):
            result, out, err = self.check()
        self.assertEqual(result, 1, out + err)
        self.assertIn("shared evidence output bound", err)
        self.assertEqual(len(budgets), 2)
        self.assertIs(budgets[0], budgets[1])
        self.assertFalse((self.evidence_dir / "validation.json").exists())


if __name__ == "__main__":
    unittest.main()
