#!/usr/bin/env python3
"""Acceptance tests for the advisory Rust coverage-summary workflow.

Run from the repository root:

  python3 .github/scripts/test_coverage_summary.py
"""

import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github" / "workflows" / "coverage.yml"
CI_WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"


class CoverageWorkflowAcceptanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = WORKFLOW.read_text(encoding="utf-8")

    def test_workflow_is_advisory_and_does_not_run_on_pull_requests(self):
        triggers = re.search(r"(?ms)^on:\n(?P<body>.*?)(?=^permissions:)", self.workflow)
        self.assertIsNotNone(triggers, "coverage workflow must declare explicit triggers")
        self.assertRegex(triggers.group("body"), r"(?m)^  push:\n    branches: \[main\]$")
        self.assertRegex(triggers.group("body"), r"(?m)^  workflow_dispatch:$")
        self.assertNotRegex(triggers.group("body"), r"(?m)^  pull_request(?:_target)?:")
        self.assertRegex(self.workflow, r"(?ms)^permissions:\n  contents: read\s*$")
        ci = CI_WORKFLOW.read_text(encoding="utf-8")
        self.assertNotIn("coverage.yml", ci, "coverage must not become a required CI gate")

    def test_summary_renders_crate_counts_and_one_decimal_percentages(self):
        summary_script = self._summary_script()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for crate, content in {
                "root": "LF:10\nLH:7\n",
                "api": "LF:4\nLH:4\n",
                "ingest": "LF:3\nLH:1\n",
                "mb": "LF:2\nLH:0\n",
            }.items():
                (root / f"coverage-{crate}.lcov").write_text(content, encoding="utf-8")
            summary = root / "summary.md"
            result = self._run_summary(summary_script, root, summary)
            rendered = summary.read_text(encoding="utf-8") if summary.exists() else ""

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("| Crate | Lines found | Lines hit | Line coverage |", rendered)
        self.assertIn("| `root` | 10 | 7 | 70.0% |", rendered)
        self.assertIn("| `api` | 4 | 4 | 100.0% |", rendered)
        self.assertIn("| `ingest` | 3 | 1 | 33.3% |", rendered)
        self.assertIn("| `mb` | 2 | 0 | 0.0% |", rendered)

    def test_missing_or_empty_report_fails_without_claiming_summary_success(self):
        summary_script = self._summary_script()
        for missing_report in ("coverage-api.lcov", "coverage-mb.lcov"):
            with self.subTest(missing_report=missing_report), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / "coverage-root.lcov").write_text("LF:1\nLH:1\n", encoding="utf-8")
                (root / "coverage-ingest.lcov").write_text("LF:1\nLH:1\n", encoding="utf-8")
                if missing_report == "coverage-mb.lcov":
                    (root / "coverage-api.lcov").write_text("LF:1\nLH:1\n", encoding="utf-8")
                summary = root / "summary.md"
                result = self._run_summary(summary_script, root, summary)
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertFalse(summary.exists() and "| `mb` |" in summary.read_text(encoding="utf-8"))

        for report_body, expected_error in (("", "missing or empty"), ("SF:src/lib.rs\n", "no instrumented lines")):
            with self.subTest(report_body=report_body), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                for crate in ("root", "api", "ingest", "mb"):
                    (root / f"coverage-{crate}.lcov").write_text("LF:1\nLH:1\n", encoding="utf-8")
                (root / "coverage-api.lcov").write_text(report_body, encoding="utf-8")
                result = self._run_summary(summary_script, root, root / "summary.md")
                self.assertNotEqual(result.returncode, 0, result.stderr)
                self.assertIn(expected_error, result.stderr)

    def test_invalid_hit_count_fails_instead_of_rendering_misleading_percentage(self):
        summary_script = self._summary_script()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for crate in ("root", "api", "ingest", "mb"):
                (root / f"coverage-{crate}.lcov").write_text("LF:2\nLH:1\n", encoding="utf-8")
            (root / "coverage-api.lcov").write_text("LF:2\nLH:3\n", encoding="utf-8")
            summary = root / "summary.md"
            result = self._run_summary(summary_script, root, summary)

        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertIn("invalid hit count", result.stderr)
        rendered = summary.read_text(encoding="utf-8") if summary.exists() else ""
        self.assertNotIn("| `api` |", rendered)

    @classmethod
    def _summary_script(cls):
        lines = cls.workflow.splitlines()
        try:
            step = lines.index("      - name: Write line coverage summary")
            run = lines.index("        run: |", step + 1)
        except ValueError as exc:
            raise AssertionError("workflow must contain the summary-generation shell step") from exc

        body = []
        for line in lines[run + 1 :]:
            if line.startswith("          "):
                body.append(line[10:])
            elif not line.strip():
                body.append("")
            else:
                break
        if not body:
            raise AssertionError("summary-generation shell step must not be empty")
        return "\n".join(body) + "\n"

    @staticmethod
    def _run_summary(script, cwd, summary):
        env = os.environ.copy()
        env["GITHUB_STEP_SUMMARY"] = str(summary)
        return subprocess.run(
            ["bash", "-e", "-o", "pipefail", "-c", script],
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
