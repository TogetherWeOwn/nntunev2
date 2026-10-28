#!/usr/bin/env python3
"""Unit tests for check_web_bundle_budget.py. Run: python3 <this file>."""

from __future__ import annotations

import contextlib
import io
import json
import os
import subprocess
import unittest
from unittest.mock import patch
import re
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_web_bundle_budget import (
    BUDGET_CLIENT_JS_BYTES,
    TOP_FILES_ON_REPORT,
    BudgetError,
    check,
    default_client_dir,
    main,
    measure,
)


_TEMP_DIRS = contextlib.ExitStack()


def _temp_dir() -> Path:
    return Path(_TEMP_DIRS.enter_context(tempfile.TemporaryDirectory()))


def _client_dir_with(sizes: dict[str, int]) -> Path:
    tmp = _temp_dir()
    for name, size in sizes.items():
        path = tmp / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"x" * size)
    return tmp


def test_under_budget_passes() -> None:
    client = _client_dir_with({"a.js": 100, "nested/b.js": 50})
    total, sized = check(client, 1_000)
    assert total == 150, total
    assert [size for _, size in sized] == [100, 50], sized
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 0


def test_over_budget_fails() -> None:
    client = _client_dir_with({"a.js": 900, "b.js": 200})
    try:
        check(client, 1_000)
    except BudgetError as exc:
        assert "exceeds budget" in str(exc), exc
    else:
        raise AssertionError("over-budget bundle must raise BudgetError")
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 1


def test_missing_dir_fails_closed() -> None:
    missing = _temp_dir() / "no-such-client"
    try:
        check(missing, 1_000)
    except BudgetError as exc:
        assert "missing" in str(exc), exc
    else:
        raise AssertionError("missing client dir must raise BudgetError")
    assert main(["--client-dir", str(missing), "--budget", "1000"]) == 1


def test_empty_bundle_fails_closed() -> None:
    client = _temp_dir()
    (client / "style.css").write_text("body {}", encoding="utf-8")
    try:
        check(client, 1_000)
    except BudgetError as exc:
        assert "empty bundle" in str(exc), exc
    else:
        raise AssertionError("empty bundle must raise BudgetError")
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 1


def test_non_positive_budget_rejected() -> None:
    client = _client_dir_with({"a.js": 10})
    for bad in (0, -100):
        try:
            check(client, bad)
        except BudgetError as exc:
            assert "positive" in str(exc), exc
        else:
            raise AssertionError(f"budget {bad} must raise BudgetError")


def test_pinned_budget_covers_current_build() -> None:
    """The pinned budget must leave headroom over a 306,065 B bundle."""
    assert BUDGET_CLIENT_JS_BYTES > 306_065, BUDGET_CLIENT_JS_BYTES


def test_pinned_budget_value_unchanged() -> None:
    """The immutable starting allocation is not edited for feature admissions."""
    assert BUDGET_CLIENT_JS_BYTES == 337_920, BUDGET_CLIENT_JS_BYTES


def test_exact_budget_passes_and_one_byte_over_fails() -> None:
    client = _client_dir_with({"a.js": 600, "nested/b.js": 400})
    total, _ = check(client, 1_000)
    assert total == 1_000, total
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 0
    # A one-byte increase must fail with an overage of exactly 1 B.
    (client / "a.js").write_bytes(b"x" * 601)
    try:
        check(client, 1_000)
    except BudgetError as exc:
        assert "over by 1 B" in str(exc), exc
    else:
        raise AssertionError("budget + 1 byte must raise BudgetError")
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 1


def test_only_js_counts_toward_total() -> None:
    client = _client_dir_with(
        {
            "a.js": 100,
            "nested/deep/c.js": 50,
            "style.css": 10_000,
            "a.js.map": 10_000,
            "notes.txt": 10_000,
            "data.json": 10_000,
            "image.png": 10_000,
        }
    )
    total, sized = check(client, 1_000)
    assert total == 150, total
    assert sorted(path.name for path, _ in sized) == ["a.js", "c.js"], sized
    assert main(["--client-dir", str(client), "--budget", "1000"]) == 0


def test_equal_size_files_sort_by_full_path() -> None:
    # Basename order (b.js < z.js) disagrees with full-path order here, so
    # this proves the tie-break uses the full path, not the file name.
    client = _client_dir_with({"b.js": 100, "a/z.js": 100})
    _, sized = check(client, 10_000)
    assert [str(path) for path, _ in sized] == sorted(
        str(path) for path, _ in sized
    ), sized
    assert sized[0][0].name == "z.js", sized


def test_failure_reports_exactly_five_largest_in_order() -> None:
    assert TOP_FILES_ON_REPORT == 5, TOP_FILES_ON_REPORT
    client = _client_dir_with(
        {
            "big.js": 900,
            "tie/b.js": 500,
            "tie/a.js": 500,
            "m1.js": 400,
            "m2.js": 300,
            "m3.js": 200,
            "m4.js": 100,
        }
    )
    budget = 1_000
    total = 900 + 500 + 500 + 400 + 300 + 200 + 100
    assert total == 2_900, total
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        assert main(["--client-dir", str(client), "--budget", str(budget)]) == 1
    lines = buf.getvalue().splitlines()
    assert f"over by {total - budget} B" in lines[0], lines
    report = [line for line in lines if line.startswith("bundle budget:   ")]
    expected = [
        f"bundle budget:   {size:>8} B  {path.name}"
        for path, size in measure(client)[:TOP_FILES_ON_REPORT]
    ]
    assert report == expected, (report, expected)
    assert len(report) == TOP_FILES_ON_REPORT, report
    # Largest first with the tie broken by full path: big, tie/a, tie/b, ...
    assert [line.split()[-1] for line in report] == [
        "big.js",
        "a.js",
        "b.js",
        "m1.js",
        "m2.js",
    ], report


def test_default_client_dir_suffix() -> None:
    """default_client_dir() must resolve under apps/web/.svelte-kit/output/client."""
    parts = default_client_dir().parts
    assert tuple(parts[-5:]) == (
        "apps",
        "web",
        ".svelte-kit",
        "output",
        "client",
    ), default_client_dir()


def test_cli_defaults_match_pinned_constants() -> None:
    """Argparse defaults use the client directory and effective policy budget."""
    import check_web_bundle_budget as mod

    seen: dict[str, object] = {}
    orig_check = mod.check

    def fake_check(client_dir: Path, budget: int) -> tuple[int, list]:
        seen["client_dir"] = client_dir
        seen["budget"] = budget
        return 0, []

    mod.check = fake_check  # type: ignore[assignment]
    try:
        with patch.object(mod, "policy_budget", return_value=BUDGET_CLIENT_JS_BYTES + 1000):
            assert mod.main([]) == 0
    finally:
        mod.check = orig_check
    assert seen["budget"] == BUDGET_CLIENT_JS_BYTES + 1000, seen
    assert seen["client_dir"] == default_client_dir(), seen


def test_cli_non_positive_budget_exits_1() -> None:
    client = _client_dir_with({"a.js": 10})
    assert main(["--client-dir", str(client), "--budget", "0"]) == 1
    assert main(["--client-dir", str(client), "--budget", "-50"]) == 1


def test_cli_non_integer_budget_exits_2() -> None:
    client = _client_dir_with({"a.js": 10})
    try:
        main(["--client-dir", str(client), "--budget", "abc"])
    except SystemExit as exc:
        assert exc.code == 2, exc.code
    else:
        raise AssertionError("non-integer --budget must raise SystemExit(2)")


def test_cli_file_as_client_dir_exits_1() -> None:
    client = _client_dir_with({"a.js": 10})
    existing_file = client / "a.js"
    assert existing_file.is_file()
    # Assert exit code only; the "missing" wording is owned elsewhere.
    assert main(["--client-dir", str(existing_file), "--budget", "1000"]) == 1


def test_empty_bundle_reports_fail_without_file_lines() -> None:
    client = _temp_dir()
    (client / "style.css").write_text("body {}", encoding="utf-8")
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        assert main(["--client-dir", str(client), "--budget", "1000"]) == 1
    lines = buf.getvalue().splitlines()
    assert lines[0].startswith("bundle budget: FAIL"), lines
    assert "no client JS files" in lines[0], lines
    file_lines = [line for line in lines if line.startswith("bundle budget:   ")]
    assert file_lines == [], file_lines


def test_missing_dir_reports_fail_without_file_lines() -> None:
    missing = _temp_dir() / "no-such-client"
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        assert main(["--client-dir", str(missing), "--budget", "1000"]) == 1
    lines = buf.getvalue().splitlines()
    assert lines[0].startswith("bundle budget: FAIL"), lines
    assert "missing" in lines[0], lines
    file_lines = [line for line in lines if line.startswith("bundle budget:   ")]
    assert file_lines == [], file_lines


def test_two_file_over_budget_reports_both_in_order() -> None:
    client = _client_dir_with({"a.js": 900, "b.js": 200})
    budget = 1_000
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        assert main(["--client-dir", str(client), "--budget", str(budget)]) == 1
    lines = buf.getvalue().splitlines()
    assert lines[0].startswith("bundle budget: FAIL"), lines
    assert "over by 100 B" in lines[0], lines
    report = [line for line in lines if line.startswith("bundle budget:   ")]
    expected = [
        f"bundle budget:   {size:>8} B  {path.name}"
        for path, size in measure(client)[:TOP_FILES_ON_REPORT]
    ]
    assert report == expected, (report, expected)
    assert len(report) == 2, report
    assert [line.split()[-1] for line in report] == ["a.js", "b.js"], report


def test_under_budget_pass_report_text() -> None:
    client = _client_dir_with({"a.js": 100, "nested/b.js": 50})
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        assert main(["--client-dir", str(client), "--budget", "1000"]) == 0
    lines = buf.getvalue().splitlines()
    assert len(lines) == 1, lines
    assert lines[0] == (
        "bundle budget: PASS: client JS total 150 B "
        "within budget 1000 B "
        "(2 files, 850 B headroom)"
    ), lines


def _repo_root() -> Path:
    return Path(__file__).resolve().parent.parent.parent


def _parse_byte_number(text: str) -> int:
    return int(text.replace(",", "").replace("_", "").strip())


def test_readme_budget_matches_pinned_constant() -> None:
    """README budget number must equal BUDGET_CLIENT_JS_BYTES."""
    readme = _repo_root().joinpath("apps", "web", "README.md").read_text(
        encoding="utf-8"
    )
    match = re.search(r"base\s+\*\*([\d,]+)\s*B", readme)
    assert match is not None, "README budget figure not found"
    readme_budget = _parse_byte_number(match.group(1))
    assert readme_budget == BUDGET_CLIENT_JS_BYTES, (
        f"README budget {readme_budget} B != "
        f"BUDGET_CLIENT_JS_BYTES {BUDGET_CLIENT_JS_BYTES} B"
    )


def test_readme_baseline_matches_script_measured_figure() -> None:
    """README baseline must equal the script's measured figure."""
    readme = _repo_root().joinpath("apps", "web", "README.md").read_text(
        encoding="utf-8"
    )
    readme_match = re.search(r"Baseline was\s+([\d,]+)\s*B", readme)
    assert readme_match is not None, "README baseline figure not found"
    readme_baseline = _parse_byte_number(readme_match.group(1))
    script_text = (
        _repo_root()
        .joinpath(".github", "scripts", "check_web_bundle_budget.py")
        .read_text(encoding="utf-8")
    )
    measured = re.findall(r"[Mm]easured\s+([\d,]+)\s*B", script_text)
    assert measured, "script measured figure not found"
    script_figures = {_parse_byte_number(item) for item in measured}
    assert len(script_figures) == 1, (
        f"script measured figures disagree: {sorted(script_figures)}"
    )
    script_baseline = next(iter(script_figures))
    assert readme_baseline == script_baseline, (
        f"README baseline {readme_baseline} B != "
        f"script measured {script_baseline} B"
    )


def test_warning_block_boundary_and_small_budgets() -> None:
    import check_web_bundle_budget as mod
    assert mod.warning_block(337_920) == 1_536
    assert mod.warning_block(1_024) == 5
    assert mod.warning_block(100) == 1
    client = _client_dir_with({"a.js": 336_383})
    for total, warns in ((336_383, False), (336_384, True), (337_920, True)):
        (client / "a.js").write_bytes(b"x" * total)
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            assert main(["--client-dir", str(client), "--budget", "337920"]) == 0
        assert ("WARN" in buf.getvalue()) == warns, buf.getvalue()


def test_ci_fetches_history_and_does_not_override_policy() -> None:
    workflow = (_repo_root() / ".github/workflows/ci.yml").read_text()
    job = workflow.split("  web-build:", 1)[1].split("  web-e2e:", 1)[0]
    assert "fetch-depth: 0" in job
    assert "--budget" not in job
    assert "check_web_bundle_budget.py" in job


class AdmissionTests(unittest.TestCase):
    """Real Git histories: dates in decisions cannot reset the rolling cap."""

    NOW = 1_800_000_000

    def setUp(self):
        import check_web_bundle_budget as mod
        self.mod = mod
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.git("init", "-q", "-b", "trunk")
        self.git("config", "user.email", "tests@example.invalid")
        self.git("config", "user.name", "Budget tests")
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        script.parent.mkdir(parents=True)
        script.write_text("BUDGET_CLIENT_JS_BYTES = 337_920\n")
        self.commit(self.NOW - 8 * 86400)
        self.base = self.git("rev-parse", "HEAD")

    def git(self, *args, timestamp=None):
        env = dict(os.environ)
        for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"):
            env.pop(key, None)
        if timestamp is not None:
            env.update(GIT_AUTHOR_DATE=f"@{timestamp} +0000",
                       GIT_COMMITTER_DATE=f"@{timestamp} +0000")
        return subprocess.check_output(
            ["git", "-C", str(self.root), *args], env=env,
            stderr=subprocess.PIPE, text=True,
        ).strip()

    def commit(self, timestamp):
        self.git("add", ".")
        self.git("commit", "-qm", "feat: synthetic feature", timestamp=timestamp)
        return self.git("rev-parse", "HEAD")

    def decision(self, name="feature", delta=1_000, **overrides):
        path = self.root / "ci/budgets/web-client-js" / f"{name}.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        record = dict(unit="web.client.js", delta_bytes=delta,
                      measured_before_bytes=337_000,
                      measured_after_bytes=337_000 + delta,
                      cause="First-party feature; checked for duplicate/heavy imports")
        record.update(overrides)
        path.write_text(json.dumps(record))
        return path

    def budget(self, base=None, now=None, merge_group=False):
        return self.mod.admitted_budget(
            self.root, base or self.base,
            now=self.NOW if now is None else now, merge_group=merge_group,
        )

    def test_no_admission_preserves_ceiling(self):
        self.assertEqual(self.budget(), 337_920)

    def test_same_diff_decision_raises_ceiling(self):
        self.decision()
        self.assertEqual(self.budget(), 338_920)

    def test_aggregate_pr_boundary_and_one_byte_over(self):
        self.decision("one", 800)
        second = self.decision("two", 889)
        self.assertEqual(self.budget(), 339_609)  # floor(337920 / 200)
        self.decision("two", 890)
        with self.assertRaisesRegex(BudgetError, "0.5%"):
            self.budget()

    def test_base_cannot_be_raised_without_decision(self):
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        script.write_text("BUDGET_CLIENT_JS_BYTES = 338_000\n")
        with self.assertRaisesRegex(BudgetError, "base"):
            self.budget()

    def test_decision_history_is_append_only(self):
        path = self.decision()
        self.base = self.commit(self.NOW - 60)
        path.write_text(path.read_text().replace("First-party", "Rewritten"))
        with self.assertRaisesRegex(BudgetError, "immutable"):
            self.budget()
        path.unlink()
        with self.assertRaisesRegex(BudgetError, "immutable"):
            self.budget()

    def test_malformed_records_fail_closed(self):
        for changes in ({"delta_bytes": 0}, {"delta_bytes": -1},
                        {"delta_bytes": True}, {"delta_bytes": 1.5},
                        {"unit": "unknown"}, {"cause": " "},
                        {"measured_before_bytes": -1},
                        {"measured_after_bytes": 900_000},
                        {"date": "2000-01-01"}):
            with self.subTest(changes=changes):
                self.decision(**changes)
                with self.assertRaises(BudgetError):
                    self.budget()

    def test_delta_cannot_exceed_measured_growth(self):
        self.decision(delta=1_000, measured_before_bytes=337_000,
                      measured_after_bytes=338_000)
        self.assertEqual(self.budget(), 338_920)
        self.decision(delta=500, measured_before_bytes=337_000,
                      measured_after_bytes=338_000)
        self.assertEqual(self.budget(), 338_420)
        self.decision(delta=1_001, measured_before_bytes=337_000,
                      measured_after_bytes=338_000)
        with self.assertRaisesRegex(BudgetError, "measured growth"):
            self.budget()

    def test_whitespace_rewrites_are_still_history_edits(self):
        path = self.decision()
        self.base = self.commit(self.NOW - 60)
        path.write_text(path.read_text() + "\n")
        with self.assertRaisesRegex(BudgetError, "immutable"):
            self.budget()

    def test_ledger_root_must_be_a_directory(self):
        directory = self.root / "ci/budgets/web-client-js"
        directory.parent.mkdir(parents=True)
        directory.write_text("not a ledger directory")
        with self.assertRaises(BudgetError):
            self.budget()

    def test_unknown_identity_and_executable_decisions_fail_closed(self):
        path = self.decision()
        path.rename(path.with_suffix(".txt"))
        with self.assertRaises(BudgetError):
            self.budget()
        path.with_suffix(".txt").unlink()
        path = self.decision()
        path.chmod(0o755)
        with self.assertRaises(BudgetError):
            self.budget()

    def test_shallow_history_and_ambiguous_queue_fail_closed(self):
        (self.root / ".git/shallow").write_text(self.base + "\n")
        with self.assertRaisesRegex(BudgetError, "full history"):
            self.budget()
        (self.root / ".git/shallow").unlink()
        self.git("checkout", "-qb", "feature")
        self.decision()
        self.commit(self.NOW - 60)
        self.git("checkout", "-qb", "queue", self.base)
        self.git("merge", "--no-ff", "-qm", "synthetic merge", "feature")
        with self.assertRaisesRegex(BudgetError, "squash"):
            self.budget(merge_group=True)

    def test_cli_uses_trusted_event_base_and_fails_closed(self):
        event = self.root / "event.json"
        for kind, data in (("pull_request", {"pull_request": {"base": {"sha": self.base}, "head": {"sha": self.base}}}),
                           ("merge_group", {"merge_group": {"base_sha": self.base}})):
            event.write_text(json.dumps(data))
            with patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": kind}), \
                 patch.object(self.mod, "__file__", str(self.root / ".github/scripts/check_web_bundle_budget.py")), \
                 patch.object(self.mod, "admitted_budget", return_value=337_920) as validate:
                self.assertEqual(self.mod.policy_budget(), 337_920)
                self.assertEqual(validate.call_args.args[1], self.base)
                self.assertEqual(validate.call_args.kwargs["merge_group"], kind == "merge_group")
        event.write_text("{}")
        with patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": "pull_request"}):
            with self.assertRaises(BudgetError):
                self.mod.policy_budget()

    def test_dispatch_and_local_use_merge_base_on_diverged_branch(self):
        self.git("branch", "main")
        self.git("checkout", "-qb", "feature")
        (self.root / "feature").write_text("feature")
        self.commit(self.NOW - 60)
        self.git("checkout", "main")
        (self.root / "main").write_text("main")
        self.commit(self.NOW - 30)
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.git("checkout", "feature")
        self.decision()
        event = self.root / "event.json"
        event.write_text("{}")
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        with patch.object(self.mod, "__file__", str(script)):
            with patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": "workflow_dispatch"}):
                self.assertEqual(self.mod.policy_budget(), 338_920)
            with patch.dict(os.environ, {"GITHUB_EVENT_PATH": "", "GITHUB_ACTIONS": "false"}):
                self.assertEqual(self.mod.policy_budget(), 338_920)

    def test_pr_uses_actual_merge_base_parent_when_event_base_is_stale(self):
        self.git("checkout", "-qb", "feature")
        self.decision("feature", 1600)
        feature = self.commit(self.NOW - 30)
        self.git("checkout", "-qb", "main", self.base)
        self.decision("other", 1600)
        self.commit(self.NOW - 20)
        self.git("merge", "--no-ff", "-qm", "synthetic merge", "feature")
        event = self.root / "event.json"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": self.base}, "head": {"sha": feature}}}))
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        with patch.object(self.mod, "__file__", str(script)), \
             patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": "pull_request"}), \
             patch.object(self.mod.time, "time", return_value=self.NOW):
            self.assertEqual(self.mod.policy_budget(), 341_120)
            event.write_text(json.dumps({"pull_request": {"base": {"sha": feature}, "head": {"sha": feature}}}))
            with self.assertRaises(BudgetError):
                self.mod.policy_budget()

    def test_duplicate_json_fields_and_symlinks_fail_closed(self):
        path = self.decision()
        path.write_text(path.read_text().replace('"delta_bytes": 1000',
                                                '"delta_bytes": 1, "delta_bytes": 1000'))
        with self.assertRaises(BudgetError):
            self.budget()
        path.unlink()
        path.symlink_to(self.root / ".github/scripts/check_web_bundle_budget.py")
        with self.assertRaises(BudgetError):
            self.budget()

    def test_weekly_aggregate_boundary_and_one_byte_over(self):
        # Four prior PRs admitted 6000 B inside seven days.
        for i in range(4):
            self.decision(f"prior-{i}", 1500)
            self.base = self.commit(self.NOW - (4 - i) * 86400)
        self.decision("current", 758)
        self.assertEqual(self.budget(), 344_678)  # floor(337920 / 50) = 6758
        self.decision("current", 759)
        with self.assertRaisesRegex(BudgetError, "2%"):
            self.budget()

    def test_window_uses_merge_time_and_includes_exact_boundary(self):
        for i in range(4):
            self.decision(f"prior-{i}", 1500)
            self.base = self.commit(self.NOW - 7 * 86400)
        self.decision("current", 1000)
        with self.assertRaisesRegex(BudgetError, "2%"):
            self.budget()
        self.assertEqual(self.budget(now=self.NOW + 1), 344_920)

    def test_future_base_commit_is_not_an_allowance_bypass(self):
        for i in range(4):
            self.decision(f"prior-{i}", 1500)
            self.base = self.commit(self.NOW + 86400)
        self.decision("current", 1000)
        with self.assertRaisesRegex(BudgetError, "2%"):
            self.budget()

    def test_queue_attributes_each_squash_not_whole_group(self):
        self.decision("first", 1600)
        self.commit(self.NOW - 60)
        self.decision("second", 1600)
        self.commit(self.NOW - 30)
        self.assertEqual(self.budget(merge_group=True), 341_120)
        self.decision("uncommitted", 1600)
        with self.assertRaises(BudgetError):
            self.budget(merge_group=True)

    def test_queue_cannot_split_one_pr_across_files(self):
        self.decision("one", 1000)
        self.decision("two", 1000)
        self.commit(self.NOW - 60)
        with self.assertRaisesRegex(BudgetError, "0.5%"):
            self.budget(merge_group=True)

    def test_queue_pending_commits_count_against_weekly_cap(self):
        for i in range(5):
            self.decision(f"pending-{i}", 1500)
            self.commit(self.NOW - (8 + i) * 86400)  # authored dates are irrelevant
        with self.assertRaisesRegex(BudgetError, "2%"):
            self.budget(merge_group=True)

    def test_merge_from_main_branch_is_valid_for_pr_dispatch_and_local(self):
        # The latest main commit is the feature merge's second parent, not
        # part of the feature's first-parent history.
        self.git("branch", "main")
        self.git("checkout", "-qb", "feature")
        self.decision("feature", 1600)
        self.commit(self.NOW - 60)
        self.git("checkout", "main")
        self.decision("main", 1600)
        main = self.commit(self.NOW - 30)
        self.git("update-ref", "refs/remotes/origin/main", main)
        self.git("checkout", "feature")
        self.git("merge", "--no-ff", "-qm", "Merge main into feature", "main")
        head = self.git("rev-parse", "HEAD")
        self.assertNotIn(main, self.git("rev-list", "--first-parent", "HEAD").splitlines())
        with self.assertRaisesRegex(BudgetError, "first-parent"):
            self.budget(base=main, merge_group=True)
        event = self.root / "event.json"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": main}, "head": {"sha": head}}}))
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        with patch.object(self.mod, "__file__", str(script)), \
             patch.object(self.mod.time, "time", return_value=self.NOW):
            for kind in ("pull_request", "workflow_dispatch", "local"):
                with self.subTest(kind=kind), patch.dict(os.environ, {
                    "GITHUB_EVENT_PATH": "" if kind == "local" else str(event),
                    "GITHUB_EVENT_NAME": kind, "GITHUB_ACTIONS": "false",
                }):
                    self.assertEqual(self.mod.policy_budget(), 341_120)

    def test_pr_synthetic_merge_accepts_head_that_merged_main(self):
        self.git("branch", "main")
        self.git("checkout", "-qb", "feature")
        self.decision("feature", 1600)
        self.commit(self.NOW - 60)
        self.git("checkout", "main")
        (self.root / "main-change").write_text("independent main work")
        main = self.commit(self.NOW - 30)
        self.git("checkout", "feature")
        self.git("merge", "--no-ff", "-qm", "Merge main into feature", "main")
        head = self.git("rev-parse", "HEAD")
        self.git("checkout", "main")
        self.git("merge", "--no-ff", "-qm", "synthetic PR merge", "feature")
        event = self.root / "event.json"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": main}, "head": {"sha": head}}}))
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        with patch.object(self.mod, "__file__", str(script)), \
             patch.object(self.mod.time, "time", return_value=self.NOW), \
             patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": "pull_request"}):
            self.assertEqual(self.mod.policy_budget(), 339_520)

    def test_unrelated_and_nonancestor_bases_fail_closed(self):
        self.git("checkout", "-qb", "unmerged-main")
        (self.root / "main-change").write_text("not merged")
        unmerged = self.commit(self.NOW - 60)
        self.git("checkout", "--orphan", "unrelated")
        unrelated = self.commit(self.NOW - 30)
        self.git("checkout", "trunk")
        for base in (unmerged, unrelated):
            with self.subTest(base=base), self.assertRaises(BudgetError):
                self.budget(base=base)

    def test_pr_rejects_event_head_not_matching_checkout(self):
        (self.root / "feature").write_text("feature")
        self.commit(self.NOW - 30)
        event = self.root / "event.json"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": self.base}, "head": {"sha": self.base}}}))
        script = self.root / ".github/scripts/check_web_bundle_budget.py"
        with patch.object(self.mod, "__file__", str(script)), \
             patch.dict(os.environ, {"GITHUB_EVENT_PATH": str(event), "GITHUB_EVENT_NAME": "pull_request"}):
            with self.assertRaises(BudgetError):
                self.mod.policy_budget()

    def test_unavailable_base_fails_closed(self):
        with self.assertRaises(BudgetError):
            self.budget(base="0" * 40)



def main_test() -> int:
    cases = (
        test_warning_block_boundary_and_small_budgets,
        test_ci_fetches_history_and_does_not_override_policy,
        test_under_budget_passes,
        test_over_budget_fails,
        test_missing_dir_fails_closed,
        test_empty_bundle_fails_closed,
        test_non_positive_budget_rejected,
        test_pinned_budget_covers_current_build,
        test_pinned_budget_value_unchanged,
        test_exact_budget_passes_and_one_byte_over_fails,
        test_only_js_counts_toward_total,
        test_equal_size_files_sort_by_full_path,
        test_failure_reports_exactly_five_largest_in_order,
        test_empty_bundle_reports_fail_without_file_lines,
        test_missing_dir_reports_fail_without_file_lines,
        test_two_file_over_budget_reports_both_in_order,
        test_under_budget_pass_report_text,
        test_readme_budget_matches_pinned_constant,
        test_readme_baseline_matches_script_measured_figure,
        test_default_client_dir_suffix,
        test_cli_defaults_match_pinned_constants,
        test_cli_non_positive_budget_exits_1,
        test_cli_non_integer_budget_exits_2,
        test_cli_file_as_client_dir_exits_1,
    )
    for case in cases:
        case()
        print(f"bundle-budget self-test: {case.__name__} ok")
    result = unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(AdmissionTests)
    )
    assert result.wasSuccessful(), "admission policy tests failed"
    print(f"bundle-budget self-test: {len(cases)}/{len(cases)} passed")
    return 0


if __name__ == "__main__":
    with _TEMP_DIRS:
        sys.exit(main_test())
