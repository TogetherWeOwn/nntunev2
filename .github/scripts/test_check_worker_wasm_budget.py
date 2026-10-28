#!/usr/bin/env python3
"""Unit tests for check_worker_wasm_budget.py. Run: python3 <this file>."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_worker_wasm_budget import (
    BUDGET_BYTES,
    TARGET,
    WORKERS,
    cargo_args,
    collect_rows,
    evaluate,
    format_bytes,
    gzip_size,
    main,
    measure_worker,
    publish,
    render_table,
    summarize_failure,
    wasm_path,
)


class BudgetTests(unittest.TestCase):
    def test_budget_is_three_million_bytes(self):
        self.assertEqual(BUDGET_BYTES, 3_000_000)


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
    def test_covers_all_seven_workers(self):
        self.assertEqual(
            [spec["worker"] for spec in WORKERS],
            ["api", "ingest", "mb", "repair", "transcode", "stream", "youtube"],
        )

    def test_wasm_names(self):
        for spec in WORKERS:
            self.assertTrue(spec["wasm"].endswith(".wasm"), spec)

    def test_audited_workers_build_locked(self):
        locked = [spec["worker"] for spec in WORKERS if spec.get("locked")]
        self.assertEqual(
            locked, ["api", "ingest", "mb", "repair", "transcode", "stream", "youtube"]
        )

    def test_repair_builds_locked_with_committed_lockfile(self):
        repair = next(s for s in WORKERS if s["worker"] == "repair")
        self.assertIn("--locked", cargo_args(repair))

    def test_cargo_args_stream_uses_package_locked(self):
        stream = next(s for s in WORKERS if s["worker"] == "stream")
        args = cargo_args(stream)
        self.assertIn("-p", args)
        self.assertIn("nntune-stream", args)
        self.assertIn("--locked", args)

    def test_cargo_args_youtube_uses_package_locked(self):
        youtube = next(s for s in WORKERS if s["worker"] == "youtube")
        args = cargo_args(youtube)
        self.assertIn("-p", args)
        self.assertIn("nntune-youtube", args)
        self.assertIn("--locked", args)

    def test_cargo_args_api_uses_manifest_locked(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        args = cargo_args(api)
        self.assertIn("--manifest-path", args)
        self.assertIn("workers/api/Cargo.toml", args)
        self.assertIn("--locked", args)

    def test_wasm_path_layout(self):
        api = next(s for s in WORKERS if s["worker"] == "api")
        stream = next(s for s in WORKERS if s["worker"] == "stream")
        youtube = next(s for s in WORKERS if s["worker"] == "youtube")
        root = Path("/repo")
        self.assertEqual(
            wasm_path(root, api),
            Path("/repo/workers/api/target") / TARGET / "release/nntune_api.wasm",
        )
        self.assertEqual(
            wasm_path(root, stream),
            Path("/repo/target") / TARGET / "release/nntune_stream.wasm",
        )
        self.assertEqual(
            wasm_path(root, youtube),
            Path("/repo/target") / TARGET / "release/nntune_youtube.wasm",
        )


class GzipTests(unittest.TestCase):
    def test_gzip_is_deterministic(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "x.wasm"
            path.write_bytes(b"\x00asm" + bytes(range(256)) * 64)
            self.assertEqual(gzip_size(path), gzip_size(path))

    def test_summarize_failure_is_single_line(self):
        summary = summarize_failure("line1\nline2\nline3\nline4\nline5\nline6", 101)
        self.assertNotIn("\n", summary)
        self.assertIn("exit 101", summary)


class EvaluateTests(unittest.TestCase):
    def _row(self, worker, compressed, status="ok"):
        return {
            "worker": worker,
            "raw": compressed,
            "compressed": compressed,
            "status": status,
        }

    def test_all_ok_under_budget_passes(self):
        rows = [self._row("api", 100), self._row("stream", 200)]
        self.assertEqual(evaluate(rows, 3_000_000), [])

    def test_exactly_at_budget_passes(self):
        self.assertEqual(evaluate([self._row("api", 100)], 100), [])

    def test_one_byte_over_fails(self):
        failures = evaluate([self._row("api", 101)], 100)
        self.assertEqual(len(failures), 1)
        self.assertIn("api", failures[0])
        self.assertIn("101", failures[0])

    def test_error_status_fails_even_without_size(self):
        rows = [
            {
                "worker": "ingest",
                "raw": None,
                "compressed": None,
                "status": "error: build failed (exit 1)",
            }
        ]
        failures = evaluate(rows, 3_000_000)
        self.assertEqual(len(failures), 1)
        self.assertIn("ingest", failures[0])

    def test_missing_artifact_status_fails(self):
        rows = [
            {
                "worker": "mb",
                "raw": None,
                "compressed": None,
                "status": "error: missing artifact",
            }
        ]
        self.assertEqual(len(rows), 1)
        self.assertTrue(evaluate(rows, 3_000_000))


class RenderTests(unittest.TestCase):
    def test_pass_table_says_pass(self):
        rows = [
            {"worker": "api", "raw": 10, "compressed": 10, "status": "ok"},
        ]
        table = render_table(rows, 3_000_000, [])
        self.assertIn("PASS", table)
        self.assertNotIn("FAIL", table)

    def test_fail_table_lists_failures(self):
        rows = [
            {"worker": "api", "raw": 10, "compressed": 10, "status": "ok"},
        ]
        table = render_table(rows, 3_000_000, ["api: too big"])
        self.assertIn("FAIL", table)
        self.assertIn("api: too big", table)

    def test_status_pipes_are_escaped(self):
        rows = [
            {
                "worker": "api",
                "raw": None,
                "compressed": None,
                "status": "error: build failed (exit 1: 1 | foo)",
            },
        ]
        table = render_table(rows, 3_000_000, ["api: boom"])
        self.assertIn("1 \\| foo", table)
        for line in table.splitlines():
            if line.startswith("| `api`"):
                # Seven structural pipes delimit the six data columns; an
                # unescaped diagnostic pipe would add an eighth.
                self.assertEqual(line.replace("\\|", "").count("|"), 7)


class MainTests(unittest.TestCase):
    def test_missing_artifacts_fail_closed(self):
        import io
        import tempfile
        from contextlib import redirect_stdout

        with tempfile.TemporaryDirectory() as tmp:
            buf = io.StringIO()
            with redirect_stdout(buf):
                code = main(["--root", tmp, "--skip-build"])
            self.assertEqual(code, 1)
            self.assertIn("FAIL", buf.getvalue())

    def test_tiny_budget_fails_on_real_artifact_shape(self):
        import io
        import tempfile
        from contextlib import redirect_stdout

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            target = root / "workers/api/target" / TARGET / "release"
            target.mkdir(parents=True)
            # Incompressible payload: gzip output stays near input size.
            import random

            rng = random.Random(1234)
            target.joinpath("nntune_api.wasm").write_bytes(
                bytes(rng.randrange(256) for _ in range(4096))
            )
            buf = io.StringIO()
            with redirect_stdout(buf):
                code = main(["--root", tmp, "--skip-build", "--budget", "100"])
            self.assertEqual(code, 1)
            self.assertIn("exceeds budget", buf.getvalue())

    def test_generous_budget_passes_fixture(self):
        import io
        import tempfile
        from contextlib import redirect_stdout

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # One passing Worker is enough to prove the zero path; the
            # other four missing Workers must still fail the run.
            target = root / "workers/mb/target" / TARGET / "release"
            target.mkdir(parents=True)
            target.joinpath("nntune_mb.wasm").write_bytes(b"\x00asm" + b"\x00" * 64)
            buf = io.StringIO()
            with redirect_stdout(buf):
                code = main(["--root", tmp, "--skip-build"])
            self.assertEqual(code, 1)
            self.assertIn("| `mb` | 68 B |", buf.getvalue())

    def test_measure_reports_missing_path(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            api = next(s for s in WORKERS if s["worker"] == "api")
            raw, compressed, detail = measure_worker(Path(tmp), api)
            self.assertIsNone(raw)
            self.assertIsNone(compressed)
            self.assertIn("missing artifact", detail)

    def test_collect_rows_never_raises_without_cargo(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            rows = collect_rows(Path(tmp), skip_build=True)
            self.assertEqual(len(rows), len(WORKERS))
            self.assertTrue(all("status" in row for row in rows))


class SummarizeFailureEdgeTests(unittest.TestCase):
    def test_empty_output_is_bare_exit(self):
        self.assertEqual(summarize_failure("", 1), "exit 1")

    def test_whitespace_only_output_is_bare_exit(self):
        self.assertEqual(summarize_failure("   \n\t\n  ", 2), "exit 2")

    def test_fewer_than_five_lines_are_joined(self):
        self.assertEqual(summarize_failure("a\nb", 3), "exit 3: a; b")
        self.assertEqual(summarize_failure("only", 4), "exit 4: only")

    def test_blank_line_gaps_are_skipped(self):
        summary = summarize_failure("line1\n\nline2\n\nline3", 1)
        self.assertEqual(summary, "exit 1: line1; line2; line3")
        self.assertNotIn(";;", summary)

    def test_tail_longer_than_300_chars_is_truncated(self):
        summary = summarize_failure("y" * 350, 9)
        tail = summary.partition(": ")[2]
        self.assertTrue(summary.startswith("exit 9: "))
        self.assertEqual(len(tail), 300)
        self.assertTrue(tail.endswith("..."))
        self.assertEqual(tail, "y" * 297 + "...")
        self.assertNotIn("\n", summary)


class PublishTests(unittest.TestCase):
    def test_appends_table_bytes_to_summary_file(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stdout
        from unittest.mock import patch

        table = "| `api` | 1 B |\n"
        with tempfile.TemporaryDirectory() as tmp:
            summary = Path(tmp) / "summary.md"
            with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}):
                buf = io.StringIO()
                with redirect_stdout(buf):
                    publish(table)
            self.assertEqual(buf.getvalue(), table)
            self.assertEqual(summary.read_text(encoding="utf-8"), table)

    def test_unwritable_summary_warns_and_preserves_verdict(self):
        import io
        import os
        import tempfile
        from contextlib import redirect_stderr, redirect_stdout
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            # Pointing GITHUB_STEP_SUMMARY at a directory makes the
            # append open() raise IsADirectoryError (an OSError).
            with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": tmp}):
                out, err = io.StringIO(), io.StringIO()
                with redirect_stdout(out), redirect_stderr(err):
                    publish("TABLE\n")
                self.assertEqual(out.getvalue(), "TABLE\n")
                self.assertIn("warning:", err.getvalue())

                # The warning must not mask the budget verdict: an empty
                # root still fails closed with exit 1.
                out2, err2 = io.StringIO(), io.StringIO()
                with redirect_stdout(out2), redirect_stderr(err2):
                    code = main(["--root", tmp, "--skip-build"])
                self.assertEqual(code, 1)
                self.assertIn("FAIL", out2.getvalue())
                self.assertIn("warning:", err2.getvalue())


class MeasureWorkerErrorTests(unittest.TestCase):
    def test_directory_artifact_is_unreadable(self):
        import tempfile
        from unittest.mock import patch

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            api = next(s for s in WORKERS if s["worker"] == "api")
            path = wasm_path(root, api)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.mkdir()
            # A directory is not a file, so is_file() would report
            # "missing artifact" before the read is attempted. Force the
            # read path to prove directory bytes surface as unreadable
            # (read_bytes on a directory raises IsADirectoryError, an
            # OSError) via the OSError arm.
            with patch.object(Path, "is_file", return_value=True):
                raw, compressed, detail = measure_worker(root, api)
            self.assertIsNone(raw)
            self.assertIsNone(compressed)
            self.assertIn("unreadable artifact", detail)

    def test_absolute_path_outside_root_falls_back(self):
        import tempfile

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            spec = {
                "worker": "api",
                "target_dir": "/definitely/outside",
                "wasm": "x.wasm",
            }
            raw, compressed, detail = measure_worker(root, spec)
            self.assertIsNone(raw)
            self.assertIsNone(compressed)
            self.assertIn("missing artifact", detail)
            # relative_to(root) raises ValueError for this absolute path,
            # so the fallback shows the absolute path itself.
            self.assertIn("/definitely/outside", detail)


class EvaluateNoneSizeTests(unittest.TestCase):
    def test_ok_status_with_none_size_passes_current_behavior(self):
        # Pinned hole: ("ok", None) silently passes today. This test pins
        # the current verdict without changing it.
        rows = [{"worker": "api", "raw": None, "compressed": None, "status": "ok"}]
        self.assertEqual(evaluate(rows, 3_000_000), [])


class RenderNewlineTests(unittest.TestCase):
    def test_newlines_are_flattened_to_single_line_row(self):
        rows = [
            {
                "worker": "api",
                "raw": None,
                "compressed": None,
                "status": "error: foo\nbar\nbaz",
            },
        ]
        table = render_table(rows, 3_000_000, ["api: boom"])
        api_lines = [line for line in table.splitlines() if line.startswith("| `api`")]
        self.assertEqual(len(api_lines), 1)
        self.assertIn("foo bar baz", api_lines[0])
        self.assertNotIn("foo\nbar", table.split("| `api` |")[1].split("|")[5])
        self.assertEqual(api_lines[0].replace("\\|", "").count("|"), 7)


class CargoArgsUnlockedTests(unittest.TestCase):
    def test_manifest_spec_without_locked_omits_flag(self):
        spec = {
            "kind": "manifest",
            "manifest": "workers/api/Cargo.toml",
            "locked": False,
        }
        args = cargo_args(spec)
        self.assertNotIn("--locked", args)
        self.assertIn("--manifest-path", args)
        self.assertIn("workers/api/Cargo.toml", args)

    def test_package_spec_without_locked_omits_flag(self):
        spec = {"kind": "package", "package": "nntune-stream", "locked": False}
        args = cargo_args(spec)
        self.assertNotIn("--locked", args)
        self.assertIn("-p", args)


if __name__ == "__main__":
    unittest.main()
