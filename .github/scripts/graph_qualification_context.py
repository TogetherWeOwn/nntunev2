"""Validate the fixed source and trusted execution context."""

import re


SHA = re.compile(r"[0-9a-f]{40}\Z")
REPOSITORY = "TogetherWeOwn/nntunev2"
PR_NUMBER = 1072
HEAD = "fe99a3cefe685b5b7fca39efe79e976f0cfc54e7"


def require_sha(value):
    if not isinstance(value, str) or not SHA.fullmatch(value):
        raise ValueError("expected a full lowercase commit object ID")
    return value


def validate_context(number, expected_base, execution_sha, control_sha, live_base, pr):
    if number != str(PR_NUMBER):
        raise ValueError("unexpected pull request")
    for value in (expected_base, execution_sha, control_sha, live_base):
        require_sha(value)
    if len({expected_base, execution_sha, control_sha, live_base}) != 1:
        raise ValueError("trusted main revision changed or does not match execution")
    if (pr.get("number") != PR_NUMBER or pr.get("state") != "open"
            or pr.get("draft") is not True
            or (pr.get("base") or {}).get("ref") != "main"
            or ((pr.get("base") or {}).get("repo") or {}).get("full_name") != REPOSITORY
            or (pr.get("head") or {}).get("sha") != HEAD
            or ((pr.get("head") or {}).get("repo") or {}).get("full_name") != REPOSITORY):
        raise ValueError("source pull request identity changed")
