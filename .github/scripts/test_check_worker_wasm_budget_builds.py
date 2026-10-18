#!/usr/bin/env python3
"""Mocked build-orchestration tests for check_worker_wasm_budget.py.

Run: python3 .github/scripts/test_check_worker_wasm_budget_builds.py

Every test that exercises the build path mocks ``subprocess.run``: no real
cargo invocation ever happens. Fixture ``.wasm`` artifacts live in per-test
temporary directories only.
"""

from __future__ import annotations

import io
import subprocess
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

import check_worker_wasm_budget as checker
from check_worker_wasm_budget import (
    BUILD_TIMEOUT_SECONDS,
    WORKERS,
    build_worker,
    cargo_args,
    collect_rows,
    main,
    wasm_path,
)

TINY_WASM = b"\x00asm" + b"\x00" * 64


def write_fixture(root: Path, worker: str) -> Path:
    """Create a tiny under-budget fixture artifact for one worker."""
    spec = next(s for s in WORKERS if s["worker"] == worker)
    path = wasm_path(root, spec)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(TINY_WASM)
    return path


def ok_proc() -> SimpleNamespace:
    return SimpleNamespace(returncode=0, stdout="ok")


def fail_proc() -> SimpleNamespace:
    return SimpleNamespace(returncode=1, stdout="error: boom")


class BuildWorkerContractTests(unittest.TestCase):
    def test_timeout_is_fail_closed(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        root = Path("/repo")
        with patch.object(
            checker.subprocess, "run", side_effect=subprocess.TimeoutExpired("cargo", 1)
        ) as fake:
            ok, detail = build_worker(root, api)
        self.assertFalse(ok)
        self.assertIn("timed out", detail)
        self.assertIn(str(BUILD_TIMEOUT_SECONDS), detail)
        fake.assert_called_once()
        argv, kwargs = fake.call_args.args[0], fake.call_args.kwargs
        self.assertEqual(argv, cargo_args(api))
        self.assertEqual(kwargs["cwd"], root)
        self.assertEqual(kwargs["timeout"], BUILD_TIMEOUT_SECONDS)
        self.assertEqual(kwargs["timeout"], 1800)

    def test_oserror_is_fail_closed(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        root = Path("/repo")
        with patch.object(
            checker.subprocess, "run", side_effect=OSError("no cargo")
        ) as fake:
            ok, detail = build_worker(root, api)
        self.assertFalse(ok)
        self.assertIn("cargo not runnable", detail)
        fake.assert_called_once()
        self.assertEqual(fake.call_args.args[0], cargo_args(api))
        self.assertEqual(fake.call_args.kwargs["cwd"], root)
        self.assertEqual(fake.call_args.kwargs["timeout"], BUILD_TIMEOUT_SECONDS)

    def test_nonzero_exit_is_fail_closed(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        root = Path("/repo")
        with patch.object(checker.subprocess, "run", return_value=fail_proc()) as fake:
            ok, detail = build_worker(root, api)
        self.assertFalse(ok)
        self.assertIn("exit 1", detail)
        fake.assert_called_once()
        self.assertEqual(fake.call_args.args[0], cargo_args(api))
        self.assertEqual(fake.call_args.kwargs["timeout"], BUILD_TIMEOUT_SECONDS)

    def test_successful_execution(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        root = Path("/repo")
        with patch.object(checker.subprocess, "run", return_value=ok_proc()) as fake:
            ok, detail = build_worker(root, api)
        self.assertTrue(ok)
        self.assertEqual(detail, "ok")
        fake.assert_called_once()
        self.assertEqual(fake.call_args.args[0], cargo_args(api))
        self.assertEqual(fake.call_args.kwargs["timeout"], BUILD_TIMEOUT_SECONDS)


class CollectRowsBuildTests(unittest.TestCase):
    def test_failed_build_ignores_stale_artifact_and_continues(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Stale artifact for the failing worker: must NOT be reused.
            write_fixture(root, "api")
            # Fresh fixture for a later worker: proves collection continues.
            write_fixture(root, "ingest")

            def fake_run(argv, **kwargs):
                self.assertEqual(kwargs["timeout"], BUILD_TIMEOUT_SECONDS)
                self.assertEqual(kwargs["cwd"], root)
                if "workers/api/Cargo.toml" in argv:
                    return fail_proc()
                return ok_proc()

            with patch.object(checker.subprocess, "run", side_effect=fake_run):
                rows = collect_rows(root, skip_build=False)

            self.assertEqual(len(rows), len(WORKERS))
            by_worker = {row["worker"]: row for row in rows}
            api_row = by_worker["api"]
            self.assertIsNone(api_row["raw"])
            self.assertIsNone(api_row["compressed"])
            self.assertIn("build failed", api_row["status"])
            # Collection continued past the failure: ingest measured ok.
            self.assertEqual(by_worker["ingest"]["status"], "ok")
            self.assertIsNotNone(by_worker["ingest"]["compressed"])

    def test_all_builds_invoked_with_existing_contract(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            seen = []

            def fake_run(argv, **kwargs):
                seen.append((list(argv), dict(kwargs)))
                return ok_proc()

            with patch.object(checker.subprocess, "run", side_effect=fake_run):
                collect_rows(root, skip_build=False)

            self.assertEqual(len(seen), len(WORKERS))
            for spec, (argv, kwargs) in zip(WORKERS, seen):
                self.assertEqual(argv, cargo_args(spec))
                self.assertEqual(kwargs["cwd"], root)
                self.assertEqual(kwargs["timeout"], BUILD_TIMEOUT_SECONDS)


class MainExitTests(unittest.TestCase):
    def test_main_returns_1_on_build_failure(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # Every worker has a valid fixture, but the api build fails, so
            # the stale/present artifact must not rescue the run.
            for spec in WORKERS:
                write_fixture(root, spec["worker"])

            def fake_run(argv, **kwargs):
                if "workers/api/Cargo.toml" in argv:
                    return fail_proc()
                return ok_proc()

            with patch.object(checker.subprocess, "run", side_effect=fake_run):
                buf = io.StringIO()
                with redirect_stdout(buf):
                    code = main(["--root", tmp])
            self.assertEqual(code, 1)
            self.assertIn("FAIL", buf.getvalue())
            self.assertIn("api", buf.getvalue())

    def test_main_returns_0_when_all_five_pass(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for spec in WORKERS:
                write_fixture(root, spec["worker"])
            with patch.object(
                checker.subprocess, "run", return_value=ok_proc()
            ) as fake:
                buf = io.StringIO()
                with redirect_stdout(buf):
                    code = main(["--root", tmp])
            self.assertEqual(code, 0)
            self.assertIn("PASS", buf.getvalue())
            self.assertEqual(fake.call_count, len(WORKERS))
            for spec in WORKERS:
                self.assertIn(f"| `{spec['worker']}` |", buf.getvalue())


if __name__ == "__main__":
    unittest.main()
