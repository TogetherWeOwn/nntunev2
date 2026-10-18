#!/usr/bin/env python3
"""Pin Wasm reporter tool and artifact I/O failure arms. Run: python3 <this file>.

Reporter arms under test (report_worker_wasm_sizes.py, warning-only):

* ``build_worker`` catches ``OSError`` from ``subprocess.run`` (cargo not
  runnable) and returns ``(False, "cargo not runnable: ...")`` instead of
  raising. Distinct from ``TimeoutExpired`` and nonzero-exit handling.
* ``measure_worker`` catches ``OSError`` from ``stat`` or read/compression
  on a present artifact and returns ``(None, None, "unreadable artifact:
  ...")``. Distinct from the missing-artifact ``(None, None, "missing
  artifact: ...")`` arm.
* ``collect_rows``/``main`` isolate one affected worker: a single error
  row with ``n/a`` sizes, later workers still measured, exit 0.

All I/O failures here are injected with synthetic ``OSError`` sentinels;
assertions check stable prefixes (``cargo not runnable:`` /
``unreadable artifact:``), never host error text.
"""

from __future__ import annotations

import io
import os
import sys
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from report_worker_wasm_sizes import (  # noqa: E402
    BUILD_TIMEOUT_SECONDS,
    WORKERS,
    build_worker,
    collect_rows,
    main,
    measure_worker,
    wasm_path,
)

CARGO_SENTINEL = "SYNTHETIC-CARGO-NOT-RUNNABLE-9c2e"
STAT_SENTINEL = "SYNTHETIC-STAT-FAILURE-4b71"
READ_SENTINEL = "SYNTHETIC-READ-FAILURE-8d05"


def _write_wasm(root: Path, spec: dict, data: bytes = b"\x00asm" + b"q" * 2048) -> Path:
    path = wasm_path(root, spec)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    return path


def _write_all_wasms(root: Path) -> None:
    for spec in WORKERS:
        _write_wasm(root, spec)


def _clean_env():
    """Environment without a job-summary target: stdout only, no warnings."""
    return {k: v for k, v in os.environ.items() if k != "GITHUB_STEP_SUMMARY"}


def _run_main(root: Path, *argv: str) -> tuple[int, str, str]:
    with patch.dict(os.environ, _clean_env(), clear=True):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = main(["--root", str(root), *argv])
    return rc, out.getvalue(), err.getvalue()


class BuildToolOSErrorTests(unittest.TestCase):
    def test_subprocess_oserror_returns_not_runnable(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        with patch(
            "report_worker_wasm_sizes.subprocess.run",
            side_effect=OSError(CARGO_SENTINEL),
        ):
            ok, detail = build_worker(Path("/repo"), api)
        self.assertFalse(ok)
        self.assertTrue(
            detail.startswith("cargo not runnable:"),
            f"stable prefix required, got: {detail!r}",
        )
        self.assertIn(CARGO_SENTINEL, detail)

    def test_subprocess_oserror_does_not_raise(self):
        # The contract is "returns (False, detail)", never an exception.
        api = next(s for s in WORKERS if s["worker"] == "api")
        try:
            with patch(
                "report_worker_wasm_sizes.subprocess.run",
                side_effect=OSError(CARGO_SENTINEL),
            ):
                result = build_worker(Path("/repo"), api)
        except OSError as exc:  # pragma: no cover - must not happen
            self.fail(f"build_worker raised OSError instead of returning: {exc}")
        self.assertEqual(len(result), 2)
        self.assertFalse(result[0])

    def test_tool_failure_uses_timeout_for_real_run(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        with patch(
            "report_worker_wasm_sizes.subprocess.run",
            side_effect=OSError(CARGO_SENTINEL),
        ) as run:
            build_worker(Path("/repo"), api)
        self.assertEqual(run.call_args.kwargs["timeout"], BUILD_TIMEOUT_SECONDS)


class ArtifactOSErrorTests(unittest.TestCase):
    def test_stat_oserror_returns_unreadable(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = next(s for s in WORKERS if s["worker"] == "mb")
            _write_wasm(root, spec)
            # Patch is_file to True so the injected stat failure lands in
            # the try block (is_file itself calls stat internally).
            with (
                patch.object(Path, "is_file", return_value=True),
                patch.object(
                    Path, "stat", side_effect=OSError(STAT_SENTINEL)
                ),
            ):
                raw, compressed, detail = measure_worker(root, spec)
        self.assertIsNone(raw)
        self.assertIsNone(compressed)
        self.assertTrue(
            detail.startswith("unreadable artifact:"),
            f"stable prefix required, got: {detail!r}",
        )
        self.assertIn(STAT_SENTINEL, detail)

    def test_read_oserror_returns_unreadable(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = next(s for s in WORKERS if s["worker"] == "mb")
            _write_wasm(root, spec)
            with patch(
                "report_worker_wasm_sizes.gzip_size",
                side_effect=OSError(READ_SENTINEL),
            ):
                raw, compressed, detail = measure_worker(root, spec)
        self.assertIsNone(raw)
        self.assertIsNone(compressed)
        self.assertTrue(
            detail.startswith("unreadable artifact:"),
            f"stable prefix required, got: {detail!r}",
        )
        self.assertIn(READ_SENTINEL, detail)

    def test_unreadable_is_distinct_from_missing(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = next(s for s in WORKERS if s["worker"] == "mb")
            _write_wasm(root, spec)
            with (
                patch.object(Path, "is_file", return_value=True),
                patch.object(
                    Path, "stat", side_effect=OSError(STAT_SENTINEL)
                ),
            ):
                _, _, unreadable = measure_worker(root, spec)
            missing_root = root / "empty-root"
            missing_root.mkdir()
            _, _, missing = measure_worker(missing_root, spec)
        self.assertTrue(missing.startswith("missing artifact:"))
        self.assertTrue(unreadable.startswith("unreadable artifact:"))
        self.assertNotEqual(unreadable, missing)
        self.assertNotIn("missing artifact", unreadable)


class MainIsolationTests(unittest.TestCase):
    def test_main_cargo_oserror_isolates_first_worker(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write_all_wasms(root)
            failing = WORKERS[0]["worker"]
            later = WORKERS[1]["worker"]

            def fake_run(*args, **kwargs):
                # First build raises OSError (cargo not runnable);
                # later builds succeed without invoking cargo.
                if fake_run.calls == 0:
                    fake_run.calls += 1
                    raise OSError(CARGO_SENTINEL)
                fake_run.calls += 1

                class Done:
                    returncode = 0
                    stdout = "ok"

                return Done()

            fake_run.calls = 0

            with patch(
                "report_worker_wasm_sizes.subprocess.run",
                side_effect=fake_run,
            ):
                rc, out, err = _run_main(root)
        self.assertEqual(rc, 0)
        self.assertEqual(err, "")
        failing_lines = [ln for ln in out.splitlines() if f"`{failing}`" in ln]
        self.assertEqual(len(failing_lines), 1)
        self.assertIn("cargo not runnable:", failing_lines[0])
        self.assertIn("n/a", failing_lines[0])
        later_lines = [ln for ln in out.splitlines() if f"`{later}`" in ln]
        self.assertEqual(len(later_lines), 1)
        self.assertIn("| ok |", later_lines[0])
        self.assertNotIn("n/a", later_lines[0])

    def test_main_stat_oserror_isolates_first_worker(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write_all_wasms(root)
            failing = WORKERS[0]["worker"]
            later = WORKERS[1]["worker"]
            failing_artifact_abs = os.path.abspath(wasm_path(root, WORKERS[0]))
            real_stat = Path.stat
            real_is_file = Path.is_file

            def fake_is_file(self, *args, **kwargs):
                # Python 3.12's is_file() calls self.stat() with no kwargs,
                # which would hit the stat mock below; bypass it so the
                # existence check succeeds and the failure lands in
                # measure_worker's try block (all artifacts exist here).
                # NOTE: os.path.abspath is lexical (no stat), so no recursion.
                if os.path.abspath(self) == failing_artifact_abs:
                    return True
                return real_is_file(self, *args, **kwargs)

            def fake_stat(self, *args, **kwargs):
                # Fail the bare stat() inside measure_worker for the victim
                # only; is_file() is patched above so no kwargs sniffing.
                # NOTE: never call .resolve() here: it stats the path and
                # would recurse into this mock. os.path.abspath is lexical.
                if os.path.abspath(self) == failing_artifact_abs:
                    raise OSError(STAT_SENTINEL)
                return real_stat(self, *args, **kwargs)

            with (
                patch.object(
                    Path, "is_file", autospec=True, side_effect=fake_is_file
                ),
                patch.object(Path, "stat", autospec=True, side_effect=fake_stat),
            ):
                rc, out, err = _run_main(root, "--skip-build")
        self.assertEqual(rc, 0)
        self.assertEqual(err, "")
        failing_lines = [ln for ln in out.splitlines() if f"`{failing}`" in ln]
        self.assertEqual(len(failing_lines), 1)
        self.assertIn("unreadable artifact:", failing_lines[0])
        self.assertIn("n/a", failing_lines[0])
        later_lines = [ln for ln in out.splitlines() if f"`{later}`" in ln]
        self.assertEqual(len(later_lines), 1)
        self.assertIn("| ok |", later_lines[0])
        self.assertNotIn("n/a", later_lines[0])

    def test_collect_rows_read_oserror_keeps_later_workers(self):
        with TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write_all_wasms(root)
            failing = WORKERS[0]["worker"]
            failing_artifact_abs = os.path.abspath(wasm_path(root, WORKERS[0]))

            import report_worker_wasm_sizes as reporter

            real_gzip = reporter.gzip_size

            def fake_gzip(path):
                # os.path.abspath is lexical (no stat); keeps this mock
                # robust even if a stat mock is ever layered on top.
                if os.path.abspath(path) == failing_artifact_abs:
                    raise OSError(READ_SENTINEL)
                return real_gzip(path)

            with patch(
                "report_worker_wasm_sizes.gzip_size", side_effect=fake_gzip
            ):
                rows = collect_rows(root, skip_build=True)
        by_worker = {row["worker"]: row for row in rows}
        self.assertTrue(
            by_worker[failing]["status"].startswith("error: unreadable artifact:"),
            by_worker[failing]["status"],
        )
        self.assertIsNone(by_worker[failing]["raw"])
        self.assertIsNone(by_worker[failing]["compressed"])
        for spec in WORKERS[1:]:
            self.assertEqual(
                by_worker[spec["worker"]]["status"], "ok", by_worker[spec["worker"]]
            )
            self.assertIsNotNone(by_worker[spec["worker"]]["raw"])
            self.assertIsNotNone(by_worker[spec["worker"]]["compressed"])


if __name__ == "__main__":
    unittest.main()
