"""Regression tests for strict graph-control accounting."""

import contextlib
import io
import unittest
from unittest import mock

import graph_control_accounting as accounting


class Samples(unittest.TestCase):
    def success(self):
        print("control output must not escape")

    def failure(self):
        self.fail("failure")

    def error(self):
        raise RuntimeError("error")

    @unittest.skip("skip")
    def skipped(self):
        pass

    @unittest.expectedFailure
    def expected_failure(self):
        self.fail("expected")

    @unittest.expectedFailure
    def unexpected_success(self):
        pass


class AccountingTests(unittest.TestCase):
    def test_default_controls_execute(self):
        counts = accounting.run_controls()
        self.assertEqual(set(accounting.CONTROLS), set(counts))
        for result in counts.values():
            self.assertGreater(result["matched"], 0)
            self.assertEqual(result["matched"], result["executed"])

    def run_sample(self, method):
        suite = unittest.TestSuite([Samples(method)])
        with mock.patch.object(accounting.unittest.defaultTestLoader, "loadTestsFromName", return_value=suite):
            return accounting.run_controls(("control",))

    def test_success_returns_counts_without_control_output(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(output):
            counts = self.run_sample("success")
        self.assertEqual({"control": {"matched": 1, "executed": 1}}, counts)
        self.assertEqual("", output.getvalue())

    def test_rejects_unsuccessful_results(self):
        for method in ("failure", "error", "skipped", "expected_failure", "unexpected_success"):
            with self.subTest(method=method), self.assertRaises(ValueError):
                self.run_sample(method)

    def test_rejects_no_matches(self):
        with mock.patch.object(accounting.unittest.defaultTestLoader, "loadTestsFromName",
                               return_value=unittest.TestSuite()):
            with self.assertRaises(ValueError):
                accounting.run_controls(("control",))

    def test_rejects_count_mismatch(self):
        suite = unittest.TestSuite([Samples("success")])
        with mock.patch.object(suite, "countTestCases", return_value=2), mock.patch.object(
                accounting.unittest.defaultTestLoader, "loadTestsFromName", return_value=suite):
            with self.assertRaises(ValueError):
                accounting.run_controls(("control",))

    def test_no_partial_output_when_later_control_fails(self):
        output = io.StringIO()
        suites = [unittest.TestSuite([Samples("success")]), unittest.TestSuite([Samples("failure")])]
        with mock.patch.object(accounting.unittest.defaultTestLoader, "loadTestsFromName", side_effect=suites), \
                contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(1, accounting.main())
        self.assertEqual("", output.getvalue())


if __name__ == "__main__":
    unittest.main()
