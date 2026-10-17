"""Acceptance tests: public-text scanning fails when an input cannot be read.

Contract under test (the Builder implements against this file; this file
is test-only and must not be edited by the Builder):

* Any input read/scan failure makes the command return nonzero, even
  when every other input is clean.
* Scanning continues for the remaining inputs, so a failure never hides
  findings in other files, and every failure is reported.
* Failure diagnostics are GitHub ``::error`` annotations that identify
  the failed input and a stable failure category token (for example
  ``read-error``, ``unreadable`` or the exception class name). OS error
  text is locale-dependent and is not a stable category.
* Diagnostics never emit file contents or raw exception details
  (no exception message, no ``Errno`` text, no decode reason).
* Unusual filenames are rendered safely for GitHub annotations: a raw
  newline (or other control character) in a filename must not split
  the annotation or spoof a second workflow command. Escaped forms
  such as ``%0A`` or a ``repr``-style backslash escape are acceptable.
* The summary distinguishes successfully scanned files from failures:
  an unreadable input is never counted as scanned, and the failure
  count is reported.
* Existing pattern rules, allowlists, exclusions, binary scanning and
  findings are preserved (positive controls below).

Seam contract (already present on main; the Builder must preserve it):
``public_text_lint.main(argv) -> int`` prints diagnostics to stdout and
returns the exit code; ``public_text_lint.scan_path(path)`` returns
``[(lineno, rule_id)]`` and lets ``OSError``/``UnicodeError`` propagate.
Injected-error tests patch ``scan_path`` because permission bits are
not portable (a root-run CI ignores them) and the current latin-1
decode cannot raise ``UnicodeError`` from real bytes.

This file is itself scanned by the lint, so manufactured violations
are built by concatenation and never written as banned literals.
No tracker ids, internal hosts or agent names appear in this file.
"""

import contextlib
import io
import os
import re
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import public_text_lint

SCRIPT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "public_text_lint.py")

# Manufactured violation, assembled so this file stays lint-clean.
VIOLATION = "TO" + "G-" + "12345"
CLEAN_TEXT = "ordinary public text with nothing banned\n"

# Synthetic sentinels: if any of these appear in scanner output, raw
# exception details or file contents leaked into a public CI log.
EXC_SENTINEL = "SYNTHETIC-EXCEPTION-DETAIL-7f3a9c"
CONTENT_SENTINEL = "SYNTHETIC-FILE-CONTENT-41b2d8"

CATEGORY_RE = re.compile(
    r"read-error|read_error|unreadable|scan-error|scan_error|decode-error|"
    r"OSError|IOError|UnicodeError|UnicodeDecodeError|FileNotFoundError|"
    r"PermissionError|IsADirectoryError|not-found|missing-input",
)


def write_file(directory, name, data):
    path = os.path.join(directory, name)
    mode = "wb" if isinstance(data, bytes) else "w"
    with open(path, mode) as fh:
        fh.write(data)
    return path


def run_main(paths, fake_scan=None):
    """Run main() over explicit paths; return (exit_code, stdout_text)."""
    buf = io.StringIO()
    if fake_scan is None:
        ctx = contextlib.nullcontext()
    else:
        ctx = mock.patch.object(public_text_lint, "scan_path", side_effect=fake_scan)
    with ctx, contextlib.redirect_stdout(buf):
        code = public_text_lint.main(["public_text_lint.py"] + list(paths))
    return code, buf.getvalue()


def raising_scan(failures):
    """Build a scan_path stand-in raising for {path: exception} only."""
    real_scan = public_text_lint.scan_path

    def _scan(path):
        if path in failures:
            raise failures[path]
        return real_scan(path)

    return _scan


def summary_lines(output):
    return [line for line in output.splitlines() if "scanned" in line]


def assert_summary_counts(testcase, output, scanned, failed):
    lines = summary_lines(output)
    testcase.assertTrue(lines, f"no summary line in output:\n{output}")
    joined = "\n".join(lines)
    testcase.assertRegex(
        joined,
        rf"scanned\D{{0,3}}{scanned}\b",
        f"summary must report {scanned} successfully scanned files:\n{output}",
    )
    testcase.assertRegex(
        joined,
        rf"(?:{failed}\s*(?:failed|unreadable|errors?|failures?)|"
        rf"(?:failed|unreadable|errors?|failures?)\D{{0,3}}{failed}\b)",
        f"summary must report {failed} failed input(s):\n{output}",
    )


def diagnostic_lines_for(output, needle):
    return [line for line in output.splitlines() if needle in line]


class TestPositiveControls(unittest.TestCase):
    """Existing behavior that must keep passing before and after."""

    def test_clean_files_exit_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            a = write_file(tmp, "a.txt", CLEAN_TEXT)
            b = write_file(tmp, "b.md", CLEAN_TEXT)
            code, output = run_main([a, b])
            self.assertEqual(0, code, output)
            self.assertNotIn("::error", output)

    def test_violating_file_exits_nonzero_with_finding(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = write_file(tmp, "bad.txt", f"see {VIOLATION} here\n")
            code, output = run_main([bad])
            self.assertNotEqual(0, code, output)
            self.assertIn("tog-id", output)
            # Findings name file:line + rule only, never matched bytes.
            self.assertNotIn(VIOLATION, output)

    def test_binary_bytes_are_still_scanned(self):
        with tempfile.TemporaryDirectory() as tmp:
            blob = write_file(
                tmp, "blob.bin", b"\x00\x01" + VIOLATION.encode() + b"\x00\x02"
            )
            code, output = run_main([blob])
            self.assertNotEqual(0, code, output)
            self.assertIn("tog-id", output)

    def test_scan_path_propagates_oserror_for_missing_file(self):
        # Seam pin: failures surface from scan_path; main() decides policy.
        with tempfile.TemporaryDirectory() as tmp:
            missing = os.path.join(tmp, "nope.txt")
            with self.assertRaises(OSError):
                public_text_lint.scan_path(missing)


class TestRealReadFailures(unittest.TestCase):
    """Deterministic offline failures from the real filesystem."""

    def test_missing_input_alone_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([missing])
            self.assertNotEqual(0, code, output)

    def test_missing_input_with_only_clean_files_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([clean, missing])
            self.assertNotEqual(0, code, output)

    def test_directory_as_input_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            subdir = os.path.join(tmp, "subdir")
            os.mkdir(subdir)
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            code, output = run_main([subdir, clean])
            self.assertNotEqual(0, code, output)
            self.assertTrue(diagnostic_lines_for(output, "subdir"), output)

    def test_missing_input_diagnostic_is_safe_error_annotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([missing])
            self.assertNotEqual(0, code, output)
            lines = diagnostic_lines_for(output, "missing.txt")
            self.assertTrue(lines, output)
            self.assertTrue(
                any(line.startswith("::error") for line in lines),
                f"read failure must be an ::error annotation, not a warning:\n{output}",
            )
            self.assertTrue(
                any(CATEGORY_RE.search(line) for line in lines),
                f"diagnostic must carry a stable failure category:\n{output}",
            )
            # Raw exception details are not a stable category and must
            # never reach a public CI log.
            self.assertNotIn("Errno", output)
            self.assertNotIn("No such file or directory", output)

    def test_every_missing_input_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            m1 = os.path.join(tmp, "missing-one.txt")
            m2 = os.path.join(tmp, "missing-two.txt")
            code, output = run_main([m1, clean, m2])
            self.assertNotEqual(0, code, output)
            self.assertTrue(diagnostic_lines_for(output, "missing-one.txt"), output)
            self.assertTrue(diagnostic_lines_for(output, "missing-two.txt"), output)


class TestInjectedReadFailures(unittest.TestCase):
    """Injected OSError/UnicodeError (permission bits are not portable)."""

    def test_injected_oserror_with_clean_files_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            victim = write_file(tmp, "victim.txt", CLEAN_TEXT)
            fake = raising_scan({victim: OSError(EXC_SENTINEL)})
            code, output = run_main([clean, victim], fake_scan=fake)
            self.assertNotEqual(0, code, output)
            self.assertTrue(diagnostic_lines_for(output, "victim.txt"), output)

    def test_injected_unicode_error_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            victim = write_file(tmp, "victim.txt", CLEAN_TEXT)
            exc = UnicodeDecodeError(
                "utf-8", b"\xff" + CONTENT_SENTINEL.encode(), 0, 1, EXC_SENTINEL
            )
            fake = raising_scan({victim: exc})
            code, output = run_main([victim], fake_scan=fake)
            self.assertNotEqual(0, code, output)
            self.assertTrue(diagnostic_lines_for(output, "victim.txt"), output)

    def test_exception_details_and_contents_never_emitted(self):
        with tempfile.TemporaryDirectory() as tmp:
            victim = write_file(tmp, "victim.txt", CONTENT_SENTINEL + "\n")
            for exc in (
                OSError(f"{EXC_SENTINEL} while reading {CONTENT_SENTINEL}"),
                UnicodeError(f"{EXC_SENTINEL} near {CONTENT_SENTINEL}"),
                PermissionError(13, EXC_SENTINEL),
            ):
                fake = raising_scan({victim: exc})
                code, output = run_main([victim], fake_scan=fake)
                self.assertNotEqual(0, code, output)
                self.assertNotIn(EXC_SENTINEL, output)
                self.assertNotIn(CONTENT_SENTINEL, output)

    def test_failure_category_is_stable_across_runs(self):
        with tempfile.TemporaryDirectory() as tmp:
            victim = write_file(tmp, "victim.txt", CLEAN_TEXT)
            fake = raising_scan({victim: OSError(EXC_SENTINEL)})
            _, out1 = run_main([victim], fake_scan=fake)
            _, out2 = run_main([victim], fake_scan=fake)
            cat1 = [m.group(0) for m in CATEGORY_RE.finditer(out1)]
            cat2 = [m.group(0) for m in CATEGORY_RE.finditer(out2)]
            self.assertTrue(cat1, f"no stable category token in:\n{out1}")
            self.assertEqual(cat1, cat2)


class TestMixedInputsKeepScanning(unittest.TestCase):
    """A failure must not hide findings, wherever it sits in the list."""

    def test_failure_first_still_reports_later_finding(self):
        with tempfile.TemporaryDirectory() as tmp:
            missing = os.path.join(tmp, "missing.txt")
            bad = write_file(tmp, "bad.txt", f"see {VIOLATION} here\n")
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            code, output = run_main([missing, bad, clean])
            self.assertNotEqual(0, code, output)
            self.assertIn("tog-id", output)
            self.assertTrue(diagnostic_lines_for(output, "bad.txt"), output)
            self.assertTrue(diagnostic_lines_for(output, "missing.txt"), output)

    def test_failure_last_still_reports_earlier_finding(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = write_file(tmp, "bad.txt", f"see {VIOLATION} here\n")
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([bad, clean, missing])
            self.assertNotEqual(0, code, output)
            self.assertIn("tog-id", output)
            self.assertTrue(diagnostic_lines_for(output, "missing.txt"), output)

    def test_injected_failure_between_clean_and_violating(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            victim = write_file(tmp, "victim.txt", CLEAN_TEXT)
            bad = write_file(tmp, "bad.txt", f"see {VIOLATION} here\n")
            fake = raising_scan({victim: OSError(EXC_SENTINEL)})
            code, output = run_main([clean, victim, bad], fake_scan=fake)
            self.assertNotEqual(0, code, output)
            self.assertIn("tog-id", output)
            self.assertTrue(diagnostic_lines_for(output, "victim.txt"), output)


class TestSafeDiagnostics(unittest.TestCase):
    """Unusual filenames must not break or spoof GitHub annotations."""

    def test_newline_in_filename_cannot_spoof_annotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            weird = os.path.join(tmp, "weird\n::error file=spoof.txt::fake.txt")
            code, output = run_main([weird])
            self.assertNotEqual(0, code, output)
            # The raw newline from the filename must not appear: the
            # annotation stays one line and no spoofed command is born.
            self.assertNotIn("weird\n", output)
            for line in output.splitlines():
                self.assertFalse(
                    line.startswith("::error file=spoof.txt"),
                    f"filename spoofed a workflow command:\n{output}",
                )
            self.assertTrue(
                "%0A" in output or r"\n" in output,
                f"newline in filename must be escaped (%0A or repr):\n{output}",
            )

    def test_percent_and_comma_in_filename_are_escaped(self):
        with tempfile.TemporaryDirectory() as tmp:
            # '%' introduces GitHub escapes and ',' ends a property
            # value; both must be encoded in the file= property.
            weird = os.path.join(tmp, "100%0Aevil,name.txt")
            code, output = run_main([weird])
            self.assertNotEqual(0, code, output)
            lines = [
                line
                for line in output.splitlines()
                if line.startswith("::error") and "evil" in line
            ]
            self.assertTrue(lines, output)
            for line in lines:
                self.assertNotIn("100%0Aevil,name.txt", line)
            self.assertTrue(
                "%25" in output,
                f"'%' in filename must be escaped as %25:\n{output}",
            )


class TestSummaryCounts(unittest.TestCase):
    """Unreadable inputs are failures, never counted as scanned."""

    def test_summary_excludes_failed_input_from_scanned_count(self):
        with tempfile.TemporaryDirectory() as tmp:
            a = write_file(tmp, "a.txt", CLEAN_TEXT)
            b = write_file(tmp, "b.txt", CLEAN_TEXT)
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([a, missing, b])
            self.assertNotEqual(0, code, output)
            assert_summary_counts(self, output, scanned=2, failed=1)

    def test_summary_with_all_inputs_failing(self):
        with tempfile.TemporaryDirectory() as tmp:
            m1 = os.path.join(tmp, "m1.txt")
            m2 = os.path.join(tmp, "m2.txt")
            code, output = run_main([m1, m2])
            self.assertNotEqual(0, code, output)
            assert_summary_counts(self, output, scanned=0, failed=2)

    def test_summary_mixed_finding_and_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            bad = write_file(tmp, "bad.txt", f"see {VIOLATION} here\n")
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            missing = os.path.join(tmp, "missing.txt")
            code, output = run_main([bad, clean, missing])
            self.assertNotEqual(0, code, output)
            assert_summary_counts(self, output, scanned=2, failed=1)


class TestCommandLine(unittest.TestCase):
    """End-to-end: the actual command CI runs, exit code included."""

    def run_cli(self, paths, cwd):
        return subprocess.run(
            [sys.executable, SCRIPT] + list(paths),
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=60,
        )

    def test_cli_clean_file_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            result = self.run_cli([clean], cwd=tmp)
            self.assertEqual(0, result.returncode, result.stdout + result.stderr)

    def test_cli_missing_input_exits_nonzero(self):
        with tempfile.TemporaryDirectory() as tmp:
            clean = write_file(tmp, "clean.txt", CLEAN_TEXT)
            missing = os.path.join(tmp, "missing.txt")
            result = self.run_cli([clean, missing], cwd=tmp)
            self.assertNotEqual(
                0, result.returncode, result.stdout + result.stderr
            )
            self.assertIn("missing.txt", result.stdout)
            self.assertNotIn("Errno", result.stdout)


if __name__ == "__main__":
    unittest.main()
