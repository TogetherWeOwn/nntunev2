"""Regression tests for strict graph-control accounting."""

import contextlib
import io
from pathlib import Path
import subprocess
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
    def test_ci_fails_if_either_suite_fails(self):
        workflow = Path(__file__).resolve().parents[1] / "workflows/ci.yml"
        line = next(line.strip() for line in workflow.read_text().splitlines()
                    if "python3 .github/scripts/test_graph_qualification_context.py" in line)
        for failing in ("test_graph_qualification_context.py", "test_graph_control_accounting.py"):
            script = ('python3() { case "$1" in *"$FAIL_TEST") return 1;; esac; }\n'
                      + line + '\nprintf "continued\\n"\n')
            result = subprocess.run(["bash", "-e", "-c", script], capture_output=True,
                                    text=True, env={"FAIL_TEST": failing})
            with self.subTest(failing=failing):
                self.assertNotEqual(0, result.returncode)
                self.assertNotIn("continued", result.stdout)

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

    def test_rejects_empty_and_duplicate_control_lists_before_loading(self):
        with mock.patch.object(accounting.unittest.defaultTestLoader, "loadTestsFromName") as load:
            for names in ((), ("control", "control")):
                with self.subTest(names=names), self.assertRaises(ValueError):
                    accounting.run_controls(names)
            load.assert_not_called()

    def test_loader_output_does_not_escape_accounting(self):
        def load(_):
            print("loader stdout")
            print("loader stderr", file=accounting.sys.stderr)
            return unittest.TestSuite([Samples("success")])

        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(accounting.unittest.defaultTestLoader, "loadTestsFromName", load), \
                contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.assertEqual({"control": {"matched": 1, "executed": 1}},
                             accounting.run_controls(("control",)))
        self.assertEqual(("", ""), (stdout.getvalue(), stderr.getvalue()))

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
