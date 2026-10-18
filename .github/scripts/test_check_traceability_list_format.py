#!/usr/bin/env python3
"""Pin traceability list-format vacuous-pass guards.

``check_traceability.parse_list_output`` (check_traceability.py:115-129)
fails closed with ``TraceabilityError`` (exit 2 via ``main``) when the
captured ``e2e:list`` output parses to zero tests ("parsed zero tests;
is this really `npm run e2e:list` output?") and when the ``Total: N
tests in M files`` line disagrees with the parsed titles/files ("Total
line says ...; the --list format may have changed"). Without these two
guards an empty or format-drifted list yields zero drifts and false
green. Temporary fixture list-output files only; checker and existing
suites unchanged.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_traceability import TraceabilityError, main, parse_list_output

LIST_TWO_FILES = """\
Listing tests:
  [staging] › alpha.spec.ts:10:3 › group › first title
  [staging] › alpha.spec.ts:20:3 › group › second title
  [staging] › beta.spec.ts:5:3 › solo title
Total: 3 tests in 2 files
"""

MATRIX_COVERING_BOTH = """\
# Matrix

| step | spec |
|---|---|
| one | `e2e/tests/alpha.spec.ts:10` "first title" |
| two | `e2e/tests/beta.spec.ts:5` |

Other refs: `e2e/helpers/util.ts`, the `e2e/README.md` "Evidence" section.
"""

EMPTY_LIST = ""

GARBAGE_LIST = """\
nothing looking like a list here
just some log noise
"""


def write_tree(root, matrix, listed):
    root = Path(root)
    (root / "docs").mkdir(parents=True, exist_ok=True)
    (root / "e2e" / "helpers").mkdir(parents=True, exist_ok=True)
    (root / "e2e" / "helpers" / "util.ts").write_text("util\n", encoding="utf-8")
    matrix_path = root / "docs" / "matrix.md"
    matrix_path.write_text(matrix, encoding="utf-8")
    list_path = root / "list.txt"
    list_path.write_text(listed, encoding="utf-8")
    return matrix_path


class ListFormatGuardTest(unittest.TestCase):
    def test_valid_list_parses_expected_counts(self):
        # Control: the harness itself passes so the failure pins below
        # prove the guards, not a broken fixture.
        files, total = parse_list_output(LIST_TWO_FILES)
        self.assertEqual(total, (3, 2))
        self.assertEqual(
            files,
            {
                "e2e/tests/alpha.spec.ts": ["first title", "second title"],
                "e2e/tests/beta.spec.ts": ["solo title"],
            },
        )

    def test_empty_list_raises_parsed_zero_tests(self):
        with self.assertRaisesRegex(
            TraceabilityError, r"parsed zero tests"
        ) as ctx:
            parse_list_output(EMPTY_LIST)
        self.assertIn("npm run e2e:list", str(ctx.exception))

    def test_garbage_list_raises_parsed_zero_tests(self):
        with self.assertRaisesRegex(
            TraceabilityError, r"parsed zero tests"
        ) as ctx:
            parse_list_output(GARBAGE_LIST)
        self.assertIn("npm run e2e:list", str(ctx.exception))

    def test_total_tests_mismatch_raises_total_line_says(self):
        listed = LIST_TWO_FILES.replace("3 tests", "99 tests")
        with self.assertRaisesRegex(
            TraceabilityError, r"Total line says"
        ) as ctx:
            parse_list_output(listed)
        self.assertIn("--list format may have changed", str(ctx.exception))

    def test_total_files_mismatch_raises_total_line_says(self):
        listed = LIST_TWO_FILES.replace("in 2 files", "in 5 files")
        with self.assertRaisesRegex(
            TraceabilityError, r"Total line says"
        ) as ctx:
            parse_list_output(listed)
        self.assertIn("--list format may have changed", str(ctx.exception))

    def test_main_zero_test_list_is_exit_2_not_zero(self):
        # End to end: a zero-test list file fails closed via main(),
        # never a vacuous exit 0 (or drift exit 1), with no traceback.
        with tempfile.TemporaryDirectory() as tmp:
            matrix_path = write_tree(tmp, MATRIX_COVERING_BOTH, EMPTY_LIST)
            list_path = os.path.join(tmp, "list.txt")
            with contextlib.redirect_stdout(
                io.StringIO()
            ) as out, contextlib.redirect_stderr(io.StringIO()) as err:
                code = main(
                    [
                        "--root",
                        tmp,
                        "--matrix",
                        str(matrix_path),
                        "--list-output",
                        list_path,
                    ]
                )
            self.assertEqual(2, code)
            self.assertNotEqual(0, code)
            self.assertNotEqual(1, code)
            combined = out.getvalue() + err.getvalue()
            self.assertIn("::error::traceability:", combined)
            self.assertIn("parsed zero tests", combined)
            self.assertNotIn("Traceback", combined)


if __name__ == "__main__":
    unittest.main()
