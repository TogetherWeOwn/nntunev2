"""Pin the serial test run that check_refresh_retry_mutants.py relies on.

The mutant check requires `<name> ... FAILED` for every expected test. With
`--nocapture` and parallel test threads, another test's panic text can land
between `test <name> ... ` and `FAILED`, so a killed mutant reads as
surviving. run() must therefore pass `--test-threads=1` to the test binary.
"""

import os
import subprocess
import sys
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import check_refresh_retry_mutants as mutants  # noqa: E402


class RunArgvTest(unittest.TestCase):
    def _argv(self, github_actions):
        env = {"GITHUB_ACTIONS": "true" if github_actions else "false"}
        with mock.patch.dict(os.environ, env), mock.patch.object(subprocess, "run") as fake:
            mutants.run(("--lib", "refresh_transport_"))
        return fake.call_args.args[0]

    def test_test_binary_runs_single_threaded(self):
        for github_actions in (True, False):
            with self.subTest(github_actions=github_actions):
                argv = self._argv(github_actions)
                sep = len(argv) - 1 - argv[::-1].index("--")
                harness = argv[sep + 1:]
                self.assertIn("--nocapture", harness)
                self.assertIn("--test-threads=1", harness)
                self.assertEqual(argv[sep - 2:sep], ["--lib", "refresh_transport_"])


if __name__ == "__main__":
    unittest.main()
