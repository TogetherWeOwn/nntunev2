"""Focused checks for the review-only Worker lock proposal."""

import importlib.util
import os
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "worker_lock_proposal", Path(__file__).with_name("worker_lock_proposal.py")
)
proposal = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proposal)


class ProposalGuards(unittest.TestCase):
    def test_refuses_non_actions(self):
        with (
            patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}),
            self.assertRaisesRegex(RuntimeError, "Actions only"),
        ):
            proposal.main()

    def test_refuses_invalid_head_before_running_commands(self):
        with (
            patch.dict(
                os.environ, {"GITHUB_ACTIONS": "true", "PR_HEAD_SHA": "invalid"}
            ),
            patch.object(proposal, "run") as run,
        ):
            with self.assertRaisesRegex(ValueError, "invalid PR head"):
                proposal.main()
            run.assert_not_called()

    def test_refuses_mismatched_checkout_before_resolution(self):
        with (
            patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "PR_HEAD_SHA": "a" * 40}),
            patch.object(proposal, "run", return_value="b" * 40) as run,
        ):
            with self.assertRaisesRegex(ValueError, "checkout does not match"):
                proposal.main()
            run.assert_called_once_with("git", "rev-parse", "HEAD", capture=True)


if __name__ == "__main__":
    unittest.main()
