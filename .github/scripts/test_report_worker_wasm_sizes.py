#!/usr/bin/env python3
"""Unit tests for report_worker_wasm_sizes.py. Run: python3 <this file>."""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))

from report_worker_wasm_sizes import (
    BUILD_TIMEOUT_SECONDS,
    TARGET,
    WORKERS,
    build_worker,
    cargo_args,
    collect_rows,
    format_bytes,
    gzip_size,
    main,
    measure_worker,
    publish,
    render_table,
    summarize_failure,
    wasm_path,
)


class FormatBytesTests(unittest.TestCase):
    def test_none_is_na(self):
        self.assertEqual(format_bytes(None), "n/a")

    def test_bytes(self):
        self.assertEqual(format_bytes(0), "0 B")
        self.assertEqual(format_bytes(512), "512 B")
        self.assertEqual(format_bytes(1023), "1023 B")

    def test_kibibytes(self):
        self.assertEqual(format_bytes(1024), "1.0 KiB")
        self.assertEqual(format_bytes(1536), "1.5 KiB")

    def test_mebibytes(self):
        self.assertEqual(format_bytes(1024 * 1024), "1.0 MiB")


class WorkerTableTests(unittest.TestCase):
    def test_covers_all_six_workers(self):
        self.assertEqual(
            [spec["worker"] for spec in WORKERS],
            ["api", "ingest", "mb", "repair", "stream", "youtube"],
        )

    def test_wasm_names(self):
        for spec in WORKERS:
            self.assertTrue(spec["wasm"].endswith(".wasm"), spec)

    def test_only_api_builds_locked(self):
        locked = [spec["worker"] for spec in WORKERS if spec.get("locked")]
        self.assertEqual(locked, ["api"])

    def test_cargo_args_stream_uses_package(self):
        stream = next(s for s in WORKERS if s["worker"] == "stream")
        args = cargo_args(stream)
        self.assertEqual(
            args,
            ["cargo", "build", "--release", "--target", TARGET,
             "-p", "nntune-stream"],
        )

    def test_cargo_args_youtube_uses_package_unlocked(self):
        youtube = next(s for s in WORKERS if s["worker"] == "youtube")
        args = cargo_args(youtube)
        self.assertEqual(
            args,
            ["cargo", "build", "--release", "--target", TARGET,
             "-p", "nntune-youtube"],
        )

    def test_cargo_args_api_locked_manifest(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        args = cargo_args(api)
        self.assertEqual(
            args,
            ["cargo", "build", "--release", "--target", TARGET,
             "--manifest-path", "workers/api/Cargo.toml", "--locked"],
        )

    def test_cargo_args_standalone_unlocked(self):
        ingest = next(s for s in WORKERS if s["worker"] == "ingest")
        self.assertNotIn("--locked", cargo_args(ingest))
        self.assertIn("workers/ingest/Cargo.toml", cargo_args(ingest))

    def test_wasm_paths(self):
        root = Path("/repo")
        api = next(s for s in WORKERS if s["worker"] == "api")
        self.assertEqual(
            wasm_path(root, api),
            Path("/repo/workers/api/target")
            / TARGET / "release" / "nntune_api.wasm",
        )
        stream = next(s for s in WORKERS if s["worker"] == "stream")
        self.assertEqual(
            wasm_path(root, stream),
            Path("/repo/target") / TARGET / "release" / "nntune_stream.wasm",
        )
        youtube = next(s for s in WORKERS if s["worker"] == "youtube")
        self.assertEqual(
            wasm_path(root, youtube),
            Path("/repo/target") / TARGET / "release" / "nntune_youtube.wasm",
        )


class MeasureTests(unittest.TestCase):
    def _write_wasm(self, root: Path, spec: dict, data: bytes) -> Path:
        path = wasm_path(root, spec)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_gzip_is_deterministic(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "x.wasm"
            path.write_bytes(b"\x00asm" + bytes(range(256)) * 64)
            self.assertEqual(gzip_size(path), gzip_size(path))

    def test_gzip_shrinks_repetitive_input(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "x.wasm"
            path.write_bytes(b"\x00" * 65536)
            self.assertLess(gzip_size(path), 65536)

    def test_measure_reports_raw_and_gzip(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = next(s for s in WORKERS if s["worker"] == "mb")
            data = b"\x00asm" + b"\x01\x02\x03" * 4096
            self._write_wasm(root, spec, data)
            raw, compressed, status = measure_worker(root, spec)
            self.assertEqual(raw, len(data))
            self.assertIsInstance(compressed, int)
            self.assertEqual(status, "ok")

    def test_measure_missing_artifact_is_error_row(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            spec = next(s for s in WORKERS if s["worker"] == "mb")
            raw, compressed, status = measure_worker(Path(tmp), spec)
            self.assertIsNone(raw)
            self.assertIsNone(compressed)
            self.assertNotEqual(status, "ok")

    def test_collect_rows_skip_build_never_raises(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = next(s for s in WORKERS if s["worker"] == "api")
            self._write_wasm(root, spec, b"\x00asm" + b"y" * 2048)
            rows = collect_rows(root, skip_build=True)
            self.assertEqual(len(rows), len(WORKERS))
            by_worker = {row["worker"]: row for row in rows}
            self.assertEqual(by_worker["api"]["status"], "ok")
            self.assertTrue(
                by_worker["ingest"]["status"].startswith("error:"),
                by_worker["ingest"]["status"],
            )


class SummarizeFailureTests(unittest.TestCase):
    def test_multiline_output_becomes_one_line(self):
        output = "line1\nline2\nline3\nline4\nline5\nline6\n"
        summary = summarize_failure(output, 101)
        self.assertNotIn("\n", summary)
        self.assertTrue(summary.startswith("exit 101: "))
        # Only the last five lines are kept.
        self.assertNotIn("line1", summary)
        self.assertIn("line6", summary)

    def test_empty_output_is_just_exit_code(self):
        self.assertEqual(summarize_failure("", 1), "exit 1")
        self.assertEqual(summarize_failure("  \n  \n", 1), "exit 1")

    def test_long_tail_is_truncated(self):
        summary = summarize_failure("x" * 500, 1)
        self.assertNotIn("\n", summary)
        self.assertLessEqual(len(summary), len("exit 1: ") + 300)
        self.assertTrue(summary.endswith("..."))


class RenderTests(unittest.TestCase):
    def test_table_lists_every_worker(self):
        rows = [
            {"worker": "api", "raw": 1000, "compressed": 500, "status": "ok"},
            {"worker": "ingest", "raw": None, "compressed": None,
             "status": "error: build failed (exit 101)"},
        ]
        table = render_table(rows)
        self.assertIn("| Worker | Raw | gzip |", table)
        self.assertIn("`api`", table)
        self.assertIn("`ingest`", table)
        self.assertIn("n/a", table)
        self.assertIn("error: build failed (exit 101)", table)

    def test_newline_status_stays_on_one_row(self):
        rows = [
            {"worker": "api", "raw": None, "compressed": None,
             "status": "error: first\nsecond"},
        ]
        table = render_table(rows)
        worker_lines = [ln for ln in table.splitlines() if "`api`" in ln]
        self.assertEqual(len(worker_lines), 1)
        self.assertIn("error: first second", worker_lines[0])

    def test_pipe_status_does_not_split_cells(self):
        rows = [
            {"worker": "mb", "raw": None, "compressed": None,
             "status": "error: build failed (exit 101:    | expected due to this)"},
        ]
        table = render_table(rows)
        header = next(ln for ln in table.splitlines() if ln.startswith("| Worker"))
        worker_lines = [ln for ln in table.splitlines() if "`mb`" in ln]
        self.assertEqual(len(worker_lines), 1)
        self.assertIn("\\|", worker_lines[0])
        # Escaped pipes are not cell separators: the row must carry the
        # same number of real pipes as the header.
        self.assertEqual(
            worker_lines[0].replace("\\|", "").count("|"),
            header.count("|"),
        )

    def test_table_marks_report_only(self):
        rows = [
            {"worker": "api", "raw": 2048, "compressed": 1024, "status": "ok"}
        ]
        table = render_table(rows)
        self.assertIn("2.0 KiB", table)
        self.assertIn("1.0 KiB", table)
        self.assertIn("report-only", table.lower())


class CollectRowsFailureIsolationTests(unittest.TestCase):
    def _write_wasm(self, root: Path, spec: dict, data: bytes) -> Path:
        path = wasm_path(root, spec)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_one_build_failure_isolated_remaining_measured(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for spec in WORKERS:
                self._write_wasm(root, spec, b"\x00asm" + b"q" * 2048)
            failing = "ingest"

            def fake_build(root_arg, spec):
                if spec["worker"] == failing:
                    return False, "exit 101: boom"
                return True, "ok"

            with patch(
                "report_worker_wasm_sizes.build_worker",
                side_effect=fake_build,
            ) as mock_build:
                rows = collect_rows(root, skip_build=False)
            self.assertEqual(len(rows), len(WORKERS))
            self.assertEqual(mock_build.call_count, len(WORKERS))
            by_worker = {row["worker"]: row for row in rows}
            self.assertTrue(
                by_worker[failing]["status"].startswith("error: build failed ("),
                by_worker[failing]["status"],
            )
            self.assertIn("exit 101: boom", by_worker[failing]["status"])
            self.assertIsNone(by_worker[failing]["raw"])
            self.assertIsNone(by_worker[failing]["compressed"])
            for spec in WORKERS:
                if spec["worker"] == failing:
                    continue
                self.assertEqual(
                    by_worker[spec["worker"]]["status"],
                    "ok",
                    by_worker[spec["worker"]],
                )
                self.assertIsNotNone(by_worker[spec["worker"]]["raw"])
                self.assertIsNotNone(by_worker[spec["worker"]]["compressed"])

    def test_main_returns_zero_with_isolated_build_failure(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stderr, redirect_stdout

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for spec in WORKERS:
                self._write_wasm(root, spec, b"\x00asm" + b"q" * 2048)

            def fake_build(root_arg, spec):
                if spec["worker"] == "mb":
                    return False, "exit 1: kaboom"
                return True, "ok"

            env = {
                k: v for k, v in os.environ.items() if k != "GITHUB_STEP_SUMMARY"
            }
            with (
                patch(
                    "report_worker_wasm_sizes.build_worker",
                    side_effect=fake_build,
                ),
                patch.dict(os.environ, env, clear=True),
            ):
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    rc = main(["--root", str(root)])
            self.assertEqual(rc, 0)
            self.assertIn("error: build failed (exit 1: kaboom)", out.getvalue())
            self.assertEqual(err.getvalue(), "")


class PublishSummaryTests(unittest.TestCase):
    def test_summary_set_appends_table_bytes(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stderr, redirect_stdout

        table = "## Worker Wasm sizes\n| a |\n"
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.md"
            summary.write_text("prior\n", encoding="utf-8")
            with patch.dict(
                os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}
            ):
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    publish(table)
            self.assertEqual(out.getvalue(), table)
            self.assertEqual(err.getvalue(), "")
            content = summary.read_text(encoding="utf-8")
            self.assertTrue(content.startswith("prior\n"))
            self.assertTrue(content.endswith(table))
            self.assertIn(table, content)

    def test_summary_unset_stdout_only_no_warning(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stderr, redirect_stdout

        table = "## Worker Wasm sizes\n| a |\n"
        with tempfile.TemporaryDirectory():
            env = {
                k: v for k, v in os.environ.items() if k != "GITHUB_STEP_SUMMARY"
            }
            with patch.dict(os.environ, env, clear=True):
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    publish(table)
            self.assertEqual(out.getvalue(), table)
            self.assertEqual(err.getvalue(), "")

    def test_summary_append_oserror_warns_and_keeps_stdout(self):
        import io
        import os
        from contextlib import redirect_stderr, redirect_stdout

        table = "## Worker Wasm sizes\n| a |\n"
        with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": "/nope/summary.md"}):
            with patch(
                "builtins.open", side_effect=OSError("disk full")
            ):
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    publish(table)
        self.assertEqual(out.getvalue(), table)
        self.assertIn("warning: could not append job summary", err.getvalue())
        self.assertIn("disk full", err.getvalue())


class MainWarningOnlyTests(unittest.TestCase):
    def _write_wasm(self, root: Path, spec: dict, data: bytes) -> Path:
        path = wasm_path(root, spec)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return path

    def test_main_returns_zero_when_summary_append_fails(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stderr, redirect_stdout

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for spec in WORKERS:
                self._write_wasm(root, spec, b"\x00asm" + b"z" * 1024)
            with patch.dict(
                os.environ, {"GITHUB_STEP_SUMMARY": str(Path(tmp) / "s.md")}
            ):
                with patch(
                    "builtins.open", side_effect=OSError("boom")
                ):
                    out, err = io.StringIO(), io.StringIO()
                    with redirect_stdout(out), redirect_stderr(err):
                        rc = main(["--root", str(root), "--skip-build"])
            self.assertEqual(rc, 0)
            self.assertIn("Worker", out.getvalue())
            self.assertIn("warning: could not append job summary", err.getvalue())
            self.assertIn("boom", err.getvalue())


class BuildWorkerTimeoutTests(unittest.TestCase):
    def test_timeout_fits_inside_ci_job(self):
        # Six Workers build sequentially; the worst case must stay under
        # the job's 35-minute timeout.
        self.assertLessEqual(BUILD_TIMEOUT_SECONDS * len(WORKERS), 30 * 60)

    def test_timeout_becomes_error_detail(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        with patch(
            "report_worker_wasm_sizes.subprocess.run",
            side_effect=subprocess.TimeoutExpired(cmd=["cargo"], timeout=1),
        ) as run:
            ok, detail = build_worker(Path("/repo"), api)
        self.assertFalse(ok)
        self.assertIn("timed out", detail)
        self.assertEqual(run.call_args.kwargs["timeout"], BUILD_TIMEOUT_SECONDS)


if __name__ == "__main__":
    unittest.main()
