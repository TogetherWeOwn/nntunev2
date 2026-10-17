#!/usr/bin/env python3
"""Unit tests for report_worker_wasm_sizes.py. Run: python3 <this file>."""

from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from report_worker_wasm_sizes import (
    TARGET,
    WORKERS,
    cargo_args,
    collect_rows,
    format_bytes,
    gzip_size,
    measure_worker,
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
    def test_covers_all_five_workers(self):
        self.assertEqual(
            [spec["worker"] for spec in WORKERS],
            ["api", "ingest", "mb", "repair", "stream"],
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

    def test_table_marks_report_only(self):
        rows = [
            {"worker": "api", "raw": 2048, "compressed": 1024, "status": "ok"}
        ]
        table = render_table(rows)
        self.assertIn("2.0 KiB", table)
        self.assertIn("1.0 KiB", table)
        self.assertIn("report-only", table.lower())


if __name__ == "__main__":
    unittest.main()
