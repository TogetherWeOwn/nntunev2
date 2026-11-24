#!/usr/bin/env python3
"""Post Actions job results and review decisions for the current PR head."""

import argparse
import re
import subprocess
import sys


REPO = "github.com/TogetherWeOwn/nntunev2"
JOB_URL = re.compile(rf"https://{re.escape(REPO)}/actions/runs/[0-9]+/job/([0-9]+)", re.I | re.ASCII)
CONCLUSIONS = {"action_required", "cancelled", "failure", "neutral", "skipped", "stale",
               "startup_failure", "success", "timed_out"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("number", type=int, help="pull request number")
    parser.add_argument("head_sha", help="full PR head SHA verified by the author")
    parser.add_argument("--job", nargs=2, action="append", default=[],
                        metavar=("CONCLUSION", "JOB_URL"), help="verified Actions job result")
    parser.add_argument("--general-review-cleared", action="store_true")
    parser.add_argument("--security-review-cleared", action="store_true")
    args = parser.parse_args(argv)
    if args.number < 1:
        parser.error("pull request number must be positive")
    if not re.fullmatch(r"[0-9a-f]{40}", args.head_sha):
        parser.error("head_sha must be the full lowercase PR head SHA")
    evidence = []
    for conclusion, url in args.job:
        match = JOB_URL.fullmatch(url)
        if conclusion not in CONCLUSIONS or match is None:
            parser.error("each --job requires an Actions conclusion and a job URL in this repository")
        evidence.append(f"- [Actions job {match[1]}]({url})  `{conclusion}`")
    if args.general_review_cleared:
        evidence.append("- Independent general review cleared this head.")
    if args.security_review_cleared:
        evidence.append("- Independent security review cleared this head.")
    if not evidence:
        parser.error("provide at least one Actions job or review decision")

    target = [str(args.number), "--repo", REPO]
    try:
        current = subprocess.run(
            ["gh", "pr", "view", *target, "--json", "headRefOid", "--jq", ".headRefOid"],
            stdout=subprocess.PIPE, text=True, check=True,
        ).stdout.strip()
        if current != args.head_sha:
            parser.error("current PR head differs from the supplied evidence SHA")

        # The SHA remains visible if the branch moves between the read and post.
        comment = f"## Verification for `{args.head_sha}`\n\n" + "\n".join(evidence) + "\n"
        subprocess.run(
            ["gh", "pr", "comment", *target, "--body-file", "-"],
            input=comment, text=True, check=True,
        )
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"Could not post PR evidence ({exc}).", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
