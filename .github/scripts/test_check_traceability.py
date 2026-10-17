#!/usr/bin/env python3
"""Unit tests for check_traceability.

All cases build throwaway matrix/list pairs in a temp directory, so the
fail-then-pass behavior is pinned without touching the real matrix: a fully
cited pair passes, while a missing spec, a stale path, a stale title, an
unknown quote, and an unparseable list each fail with the drift named.
"""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_traceability import check, main, parse_list_output, parse_matrix

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

```sh
grep -n "test('" e2e/tests/*.spec.ts
```
"""


def write_tree(root, matrix, listed):
    root = Path(root)
    (root / "docs").mkdir(parents=True, exist_ok=True)
    (root / "e2e" / "helpers").mkdir(parents=True, exist_ok=True)
    (root / "e2e" / "README.md").write_text("readme\n", encoding="utf-8")
    (root / "e2e" / "helpers" / "util.ts").write_text("util\n", encoding="utf-8")
    matrix_path = root / "docs" / "matrix.md"
    matrix_path.write_text(matrix, encoding="utf-8")
    list_path = root / "list.txt"
    list_path.write_text(listed, encoding="utf-8")
    return matrix_path


class CheckTraceabilityTest(unittest.TestCase):
    def run_check(self, matrix, listed):
        with tempfile.TemporaryDirectory() as tmp:
            matrix_path = write_tree(tmp, matrix, listed)
            with open(os.path.join(tmp, "list.txt"), encoding="utf-8") as fh:
                return check(Path(tmp), matrix_path, fh.read())

    def test_match_passes(self):
        self.assertEqual(
            self.run_check(MATRIX_COVERING_BOTH, LIST_TWO_FILES), []
        )

    def test_missing_spec_fails_naming_file_and_title(self):
        listed = LIST_TWO_FILES.replace(
            "Total: 3 tests in 2 files", "Total: 4 tests in 3 files"
        ) + ("  [staging] › gamma.spec.ts:7:3 › probe group › dummy probe title\n")
        drifts = self.run_check(MATRIX_COVERING_BOTH, listed)
        self.assertEqual(len(drifts), 1)
        self.assertIn("e2e/tests/gamma.spec.ts", drifts[0])
        self.assertIn("dummy probe title", drifts[0])

    def test_stale_spec_path_fails(self):
        matrix = MATRIX_COVERING_BOTH + "`e2e/tests/vanished.spec.ts`\n"
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(len(drifts), 1)
        self.assertIn("e2e/tests/vanished.spec.ts", drifts[0])

    def test_stale_helper_path_fails(self):
        matrix = MATRIX_COVERING_BOTH + "`e2e/helpers/gone.ts`\n"
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(len(drifts), 1)
        self.assertIn("e2e/helpers/gone.ts", drifts[0])

    def test_line_suffix_is_informational(self):
        # :999 names no real line, but only the file is checked.
        matrix = MATRIX_COVERING_BOTH.replace(
            "`e2e/helpers/util.ts`", "`e2e/helpers/util.ts:999`"
        )
        self.assertEqual(self.run_check(matrix, LIST_TWO_FILES), [])

    def test_stale_title_fails(self):
        matrix = MATRIX_COVERING_BOTH + '"renamed away title"\n'
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(len(drifts), 1)
        self.assertIn("renamed away title", drifts[0])

    def test_unknown_quote_fails(self):
        matrix = MATRIX_COVERING_BOTH + 'A new "Prose Section" reference.\n'
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(len(drifts), 1)
        self.assertIn("Prose Section", drifts[0])

    def test_known_non_test_quote_passes(self):
        # "Evidence" is a README section name, recorded in NON_TEST_QUOTES.
        self.assertEqual(self.run_check(MATRIX_COVERING_BOTH, LIST_TWO_FILES), [])

    def test_bare_spec_filename_counts_as_cited(self):
        matrix = MATRIX_COVERING_BOTH.replace(
            "`e2e/tests/beta.spec.ts:5`", "`beta.spec.ts:5`"
        )
        self.assertEqual(self.run_check(matrix, LIST_TWO_FILES), [])

    def test_unparseable_list_fails_closed(self):
        with self.assertRaises(Exception):
            parse_list_output("nothing looking like a list here\n")

    def test_total_mismatch_fails_closed(self):
        with self.assertRaises(Exception):
            parse_list_output(LIST_TWO_FILES.replace("3 tests", "99 tests"))

    def test_fences_stay_inert(self):
        matrix = MATRIX_COVERING_BOTH + '```\n"not a title" `e2e/tests/ghost.spec.ts`\n```\n'
        self.assertEqual(self.run_check(matrix, LIST_TWO_FILES), [])

    def test_parse_matrix_skips_globs_and_dirs(self):
        _, paths, _ = parse_matrix("`e2e/tests/*.spec.ts` and `e2e/`\n")
        self.assertEqual(paths, set())

    def test_main_missing_file_is_exit_2(self):
        with tempfile.TemporaryDirectory() as tmp:
            matrix_path = write_tree(tmp, MATRIX_COVERING_BOTH, LIST_TWO_FILES)
            list_path = os.path.join(tmp, "list.txt")
            self.assertEqual(
                main(
                    [
                        "--root",
                        tmp,
                        "--matrix",
                        str(matrix_path),
                        "--list-output",
                        list_path,
                    ]
                ),
                0,
            )
            self.assertEqual(
                main(
                    [
                        "--root",
                        tmp,
                        "--matrix",
                        str(matrix_path),
                        "--list-output",
                        os.path.join(tmp, "nope.txt"),
                    ]
                ),
                2,
            )


if __name__ == "__main__":
    unittest.main()
