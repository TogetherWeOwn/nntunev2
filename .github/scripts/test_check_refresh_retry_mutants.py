"""Pin how check_refresh_retry_mutants.py decides a mutant was killed.

Under `--nocapture`, panic text lands between `test <name> ... ` and `FAILED`:
from the test's own thread when serial, from another test when parallel. The
check therefore reads libtest's closing `failures:` list instead of the
per-test result line.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_refresh_retry_mutants as mutants  # noqa: E402

MARKER = "refresh_transport_marker_prevents_empty_upload_downgrade"
SPELLING = "refresh_transport_accepts_each_marker_spelling_for_recovery"
LEGACY = "refresh_transport_requires_marked_recovery_but_preserves_legacy_empty_refresh"

# Serial run: the test's own panic splits its result line (PR #856, first CI run).
SERIAL = """running 1 test
test handler_through_production_store_preserves_retry_secret_isolation ... thread 'handler_through_production_store_preserves_retry_secret_isolation' (3607) panicked at tests/session_refresh_postgres.rs:271:5:
assertion `left == right` failed
FAILED

failures:

failures:
    handler_through_production_store_preserves_retry_secret_isolation

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.17s
"""

# Parallel run: one test's panic splits another's result line (PR #853).
PARALLEL = f"""running 3 tests
test handlers::tests::{MARKER} ... thread 'handlers::tests::{SPELLING}' panicked at src/handlers.rs:10:5:
assertion failed
FAILED
test handlers::tests::{SPELLING} ... FAILED
test handlers::tests::{LEGACY} ... ok

failures:

failures:
    handlers::tests::{MARKER}
    handlers::tests::{SPELLING}

test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 40 filtered out; finished in 0.01s
"""


class FailedTestsTest(unittest.TestCase):
    def test_serial_panic_inside_result_line_still_counts(self):
        failed = mutants.failed_tests(SERIAL)
        self.assertTrue(mutants.killed_by(
            "handler_through_production_store_preserves_retry_secret_isolation", failed))

    def test_parallel_interleave_still_counts_every_name(self):
        failed = mutants.failed_tests(PARALLEL)
        self.assertTrue(mutants.killed_by(MARKER, failed))
        self.assertTrue(mutants.killed_by(SPELLING, failed))

    def test_passing_test_is_not_killed(self):
        self.assertFalse(mutants.killed_by(LEGACY, mutants.failed_tests(PARALLEL)))

    def test_name_must_match_a_whole_path_segment(self):
        failed = {f"handlers::tests::x_{MARKER}"}
        self.assertFalse(mutants.killed_by(MARKER, failed))

    def test_no_failures_list_means_nothing_killed(self):
        self.assertEqual(mutants.failed_tests("test result: ok. 3 passed;\n"), set())


if __name__ == "__main__":
    unittest.main()
