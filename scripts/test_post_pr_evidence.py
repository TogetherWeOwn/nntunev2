import contextlib
import io
import subprocess
import unittest
from unittest.mock import patch

import post_pr_evidence


SHA = "a" * 40
TARGET = ["42", "--repo", "github.com/TogetherWeOwn/nntunev2"]
VIEW = ["gh", "pr", "view", *TARGET, "--json", "headRefOid", "--jq", ".headRefOid"]
COMMENT = ["gh", "pr", "comment", *TARGET, "--body-file", "-"]
EVIDENCE = "- `ci-ok` passed.\n- Both T0 reviews cleared this head.\n"


class PostPrEvidence(unittest.TestCase):
    def setUp(self):
        self.run_gh = self.enterContext(patch.object(post_pr_evidence.subprocess, "run"))
        self.run_gh.return_value.stdout = SHA + "\n"
        self.enterContext(patch.object(post_pr_evidence.sys, "stdin", io.StringIO(EVIDENCE)))

    def test_posts_a_new_sha_labeled_comment_without_editing_the_pr(self):
        self.assertEqual(post_pr_evidence.main(["42", SHA]), 0)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])
        self.assertEqual(self.run_gh.call_args.kwargs["input"],
                         f"## Verification for `{SHA}`\n\n{EVIDENCE}")

    def test_stale_head_does_not_post(self):
        self.run_gh.return_value.stdout = "b" * 40 + "\n"
        with contextlib.redirect_stderr(io.StringIO()) as stderr, self.assertRaises(SystemExit):
            post_pr_evidence.main(["42", SHA])
        self.assertIn("current PR head differs", stderr.getvalue())
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW])

    def test_invalid_inputs_make_no_github_calls(self):
        with contextlib.redirect_stderr(io.StringIO()):
            for args, body in ((["0", SHA], EVIDENCE), (["42", "abc"], EVIDENCE),
                               (["42", SHA], " \n")):
                with self.subTest(args=args, body=body), patch.object(
                        post_pr_evidence.sys, "stdin", io.StringIO(body)), self.assertRaises(SystemExit):
                    post_pr_evidence.main(args)
        self.run_gh.assert_not_called()

    def test_failed_post_returns_failure_without_retrying_or_editing(self):
        self.run_gh.side_effect = [self.run_gh.return_value, subprocess.CalledProcessError(1, COMMENT)]
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(post_pr_evidence.main(["42", SHA]), 1)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])


if __name__ == "__main__":
    unittest.main()
