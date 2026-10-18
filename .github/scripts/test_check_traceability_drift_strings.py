#!/usr/bin/env python3
"""Pin traceability drift-line strings and uncited-titles truncation.

``check_traceability.check`` (check_traceability.py:183-210) routes
attribution through four exact drift formats: one uncited-spec line that
shows at most five titles plus ``(+N more)``, one stale-spec line, one
stale-path line, and one stale-title line. Existing suites only assert
substrings (file or title present), so a reworded format still passes.
These pins assert the full lines exactly, so a format change goes red.
Temporary fixture dirs only; checker and existing suites unchanged.
"""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from check_traceability import check

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

GAMMA_TITLES = [
    "gamma one",
    "gamma two",
    "gamma three",
    "gamma four",
    "gamma five",
    "gamma six",
    "gamma seven",
]


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


class DriftStringsTest(unittest.TestCase):
    def run_check(self, matrix, listed):
        with tempfile.TemporaryDirectory() as tmp:
            matrix_path = write_tree(tmp, matrix, listed)
            with open(os.path.join(tmp, "list.txt"), encoding="utf-8") as fh:
                return check(Path(tmp), matrix_path, fh.read())

    def test_seven_title_uncited_spec_truncates_to_five_plus_two_more(self):
        lines = ["Listing tests:"]
        lines.append("  [staging] › alpha.spec.ts:10:3 › group › first title")
        lines.append("  [staging] › alpha.spec.ts:20:3 › group › second title")
        lines.append("  [staging] › beta.spec.ts:5:3 › solo title")
        for i, title in enumerate(GAMMA_TITLES, start=1):
            lines.append(f"  [staging] › gamma.spec.ts:{i}:3 › group › {title}")
        lines.append("Total: 10 tests in 3 files")
        listed = "\n".join(lines) + "\n"
        drifts = self.run_check(MATRIX_COVERING_BOTH, listed)
        self.assertEqual(
            drifts,
            [
                "uncited spec file e2e/tests/gamma.spec.ts (7 tests: "
                "'gamma one', 'gamma two', 'gamma three', 'gamma four', "
                "'gamma five' (+2 more)); cite it in the matrix"
            ],
        )

    def test_stale_spec_line_is_exact(self):
        matrix = MATRIX_COVERING_BOTH + "`e2e/tests/vanished.spec.ts`\n"
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(
            drifts,
            [
                "stale spec citation e2e/tests/vanished.spec.ts: cited in "
                "the matrix but absent from `e2e:list` output "
                "(moved, renamed, or deleted)"
            ],
        )

    def test_stale_path_line_is_exact(self):
        matrix = MATRIX_COVERING_BOTH + "`e2e/helpers/gone.ts`\n"
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(
            drifts,
            [
                "stale path citation `e2e/helpers/gone.ts`: cited in the "
                "matrix but missing on disk"
            ],
        )

    def test_stale_title_line_is_exact(self):
        matrix = MATRIX_COVERING_BOTH + '"renamed away title"\n'
        drifts = self.run_check(matrix, LIST_TWO_FILES)
        self.assertEqual(
            drifts,
            [
                'stale title citation "renamed away title": cited in the '
                "matrix but no `e2e:list` test carries that title (retitled?)"
            ],
        )


if __name__ == "__main__":
    unittest.main()
