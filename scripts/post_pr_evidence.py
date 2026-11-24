#!/usr/bin/env python3
"""Post verification Markdown to a PR conversation with its reviewed head SHA."""

import argparse
import re
import subprocess
import sys


REPO = "github.com/TogetherWeOwn/nntunev2"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("number", type=int, help="pull request number")
    parser.add_argument("head_sha", help="full PR head SHA verified by the author")
    args = parser.parse_args(argv)
    if args.number < 1:
        parser.error("pull request number must be positive")
    if not re.fullmatch(r"[0-9a-f]{40}", args.head_sha):
        parser.error("head_sha must be the full lowercase PR head SHA")
    evidence = sys.stdin.read().strip()
    if not evidence:
        parser.error("provide verification Markdown on stdin")

    target = [str(args.number), "--repo", REPO]
    try:
        current = subprocess.run(
            ["gh", "pr", "view", *target, "--json", "headRefOid", "--jq", ".headRefOid"],
            stdout=subprocess.PIPE, text=True, check=True,
        ).stdout.strip()
        if current != args.head_sha:
            parser.error("current PR head differs from the supplied evidence SHA")

        # The SHA remains visible if the branch moves between the read and post.
        comment = f"## Verification for `{args.head_sha}`\n\n{evidence}\n"
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
