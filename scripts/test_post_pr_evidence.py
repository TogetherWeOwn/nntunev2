import contextlib
import io
import subprocess
import unittest
from unittest.mock import patch

import post_pr_evidence


SHA = "a" * 40
TESTED_SHA = "b" * 40
TARGET = ["42", "--repo", "github.com/TogetherWeOwn/nntunev2"]
VIEW = ["gh", "pr", "view", *TARGET, "--json", "headRefOid", "--jq", ".headRefOid"]
COMMENT = ["gh", "pr", "comment", *TARGET, "--body-file", "-"]
RUN = "https://github.com/TogetherWeOwn/nntunev2/actions/runs/123"
JOB = RUN + "/job/456"
ARGS = ["42", SHA, "--job", "success", JOB + "?pr=42", TESTED_SHA]


class PostPrEvidence(unittest.TestCase):
    def setUp(self):
        self.run_gh = self.enterContext(patch.object(post_pr_evidence.subprocess, "run"))
        self.run_gh.return_value.stdout = SHA + "\n"

    def test_posts_a_new_sha_labeled_comment_without_editing_the_pr(self):
        args = ARGS + ["--job", "skipped", RUN + "/job/457", TESTED_SHA]
        self.assertEqual(post_pr_evidence.main(args), 0)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])
        self.assertEqual(self.run_gh.call_args.kwargs["input"], f"## Verification for `{SHA}`\n\n"
                         f"- [Actions run 123]({RUN}) · [job 456]({JOB}) · `success` · tested `{TESTED_SHA}`\n"
                         f"- [Actions run 123]({RUN}) · [job 457]({RUN}/job/457)"
                         f" · `skipped` · tested `{TESTED_SHA}`\n")

    def test_posts_review_decisions_without_repeating_job_results(self):
        args = ["42", SHA, "--general-review-cleared", "--security-review-cleared"]
        self.assertEqual(post_pr_evidence.main(args), 0)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])
        self.assertEqual(self.run_gh.call_args.kwargs["input"], f"## Verification for `{SHA}`\n\n"
                         "- Independent general review cleared this head.\n"
                         "- Independent security review cleared this head.\n")

    def test_stale_head_does_not_post(self):
        self.run_gh.return_value.stdout = TESTED_SHA + "\n"
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
        for conclusion, url, tested_sha, rejected in (
                (marker, JOB, TESTED_SHA, marker),
                ("success", JOB + "?pr=42&note=" + marker, TESTED_SHA, marker),
                ("success", JOB.replace("github.com", "example.com"), TESTED_SHA, "example.com"),
                ("success", JOB.replace("nntunev2", "other-repo"), TESTED_SHA, "other-repo"),
                ("success", JOB, marker, marker)):
            with self.subTest(rejected=rejected), contextlib.redirect_stderr(io.StringIO()) as stderr:
                with self.assertRaises(SystemExit):
                    post_pr_evidence.main(["42", SHA, "--job", conclusion, url, tested_sha])
                self.assertIn("requires an Actions conclusion, a job URL", stderr.getvalue())
                self.assertNotIn(rejected, stderr.getvalue())
        self.run_gh.assert_not_called()

    def test_failed_post_returns_failure_without_retrying_or_editing(self):
        self.run_gh.side_effect = [self.run_gh.return_value, subprocess.CalledProcessError(1, COMMENT)]
        with contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(post_pr_evidence.main(ARGS), 1)
        self.assertEqual([call.args[0] for call in self.run_gh.call_args_list], [VIEW, COMMENT])


if __name__ == "__main__":
    unittest.main()
