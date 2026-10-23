#!/usr/bin/env python3
"""Unit tests for check_web_bundle_budget.py. Run: python3 <this file>."""

from __future__ import annotations

import contextlib
import io
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


def _client_dir_with(sizes: dict[str, int]) -> Path:
    tmp = Path(tempfile.mkdtemp())
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
    missing = Path(tempfile.mkdtemp()) / "no-such-client"
    try:
        check(missing, 1_000)
    except BudgetError as exc:
        assert "missing" in str(exc), exc
    else:
        raise AssertionError("missing client dir must raise BudgetError")
    assert main(["--client-dir", str(missing), "--budget", "1000"]) == 1


def test_empty_bundle_fails_closed() -> None:
    client = Path(tempfile.mkdtemp())
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
    """The pinned budget must leave headroom over a 229,997 B bundle."""
    assert BUDGET_CLIENT_JS_BYTES > 229_997, BUDGET_CLIENT_JS_BYTES


def test_pinned_budget_value_unchanged() -> None:
    """Pin the exact budget value: bumps happen deliberately, in their own PR."""
    assert BUDGET_CLIENT_JS_BYTES == 307_200, BUDGET_CLIENT_JS_BYTES


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
    """Argparse defaults must be default_client_dir() and BUDGET_CLIENT_JS_BYTES."""
    import check_web_bundle_budget as mod

    seen: dict[str, object] = {}
    orig_check = mod.check

    def fake_check(client_dir: Path, budget: int) -> tuple[int, list]:
        seen["client_dir"] = client_dir
        seen["budget"] = budget
        return 0, []

    mod.check = fake_check  # type: ignore[assignment]
    try:
        assert mod.main([]) == 0
    finally:
        mod.check = orig_check
    assert seen["budget"] == BUDGET_CLIENT_JS_BYTES, seen
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
    client = Path(tempfile.mkdtemp())
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
    missing = Path(tempfile.mkdtemp()) / "no-such-client"
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
    match = re.search(r"exceeds\s+\*\*([\d,]+)\s*B", readme)
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


def main_test() -> int:
    cases = (
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
    print(f"bundle-budget self-test: {len(cases)}/{len(cases)} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main_test())
