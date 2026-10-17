#!/usr/bin/env python3
"""Unit tests for check_web_bundle_budget.py. Run: python3 <this file>."""

from __future__ import annotations

import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_web_bundle_budget import (
    BUDGET_CLIENT_JS_BYTES,
    BudgetError,
    check,
    main,
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
    """The pinned budget must leave headroom over a 176,417 B bundle."""
    assert BUDGET_CLIENT_JS_BYTES > 176_417, BUDGET_CLIENT_JS_BYTES


def main_test() -> int:
    cases = (
        test_under_budget_passes,
        test_over_budget_fails,
        test_missing_dir_fails_closed,
        test_empty_bundle_fails_closed,
        test_non_positive_budget_rejected,
        test_pinned_budget_covers_current_build,
    )
    for case in cases:
        case()
        print(f"bundle-budget self-test: {case.__name__} ok")
    print(f"bundle-budget self-test: {len(cases)}/{len(cases)} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main_test())
