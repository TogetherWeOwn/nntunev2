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
JOB = "https://github.com/TogetherWeOwn/nntunev2/actions/runs/123/job/456"
ARGS = ["42", SHA, "--job", "success", JOB]


class PostPrEvidence(unittest.TestCase):
    def setUp(self):
        self.run_gh = self.enterContext(patch.object(post_pr_evidence.subprocess, "run"))
        self.run_gh.return_value.stdout = SHA + "\n"

    def test_posts_a_new_sha_labeled_comment_without_editing_the_pr(self):
        args = ARGS + ["--job", "skipped", JOB.replace("456", "457"),
                       "--general-review-cleared", "--security-review-cleared"]
        self.assertEqual(post_pr_evidence.main(args), 0)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])
        self.assertEqual(self.run_gh.call_args.kwargs["input"], f"## Verification for `{SHA}`\n\n"
                         f"- [Actions job 456]({JOB})  `success`\n"
                         f"- [Actions job 457]({JOB.replace('456', '457')})  `skipped`\n"
                         "- Independent general review cleared this head.\n"
                         "- Independent security review cleared this head.\n")

    def test_stale_head_does_not_post(self):
        self.run_gh.return_value.stdout = "b" * 40 + "\n"
        with contextlib.redirect_stderr(io.StringIO()) as stderr, self.assertRaises(SystemExit):
            post_pr_evidence.main(ARGS)
        self.assertIn("current PR head differs", stderr.getvalue())
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW])

    def test_invalid_inputs_make_no_github_calls(self):
        with contextlib.redirect_stderr(io.StringIO()):
            for args in (["0", *ARGS[1:]], ["42", "abc", *ARGS[2:]], ["42", SHA]):
                with self.subTest(args=args), self.assertRaises(SystemExit):
                    post_pr_evidence.main(args)
        self.run_gh.assert_not_called()

    def test_unapproved_evidence_is_refused_without_echoing_or_posting(self):
        marker = "TOG-" + "123"
        for conclusion, url, rejected in (
                (marker, JOB, marker),
                ("success", JOB + "?note=" + marker, marker),
                ("success", JOB.replace("github.com", "example.com"), "example.com"),
                ("success", JOB.replace("nntunev2", "other-repo"), "other-repo")):
            with self.subTest(rejected=rejected), contextlib.redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaises(SystemExit):
                    post_pr_evidence.main(["42", SHA, "--job", conclusion, url])
                self.assertIn("requires an Actions conclusion and a job URL", stderr.getvalue())
                self.assertNotIn(rejected, stderr.getvalue())
        self.run_gh.assert_not_called()

    def test_failed_post_returns_failure_without_retrying_or_editing(self):
        self.run_gh.side_effect = [self.run_gh.return_value, subprocess.CalledProcessError(1, COMMENT)]
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(post_pr_evidence.main(ARGS), 1)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])


if __name__ == "__main__":
    unittest.main()
