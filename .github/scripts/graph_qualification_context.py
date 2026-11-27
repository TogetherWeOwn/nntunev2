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


def _field(record, key):
    return record.get(key) if isinstance(record, dict) else None


def validate_context(number, expected_base, execution_sha, control_sha, live_base, pr):
    if number != str(PR_NUMBER):
        raise ValueError("unexpected pull request")
    for value in (expected_base, execution_sha, control_sha, live_base):
        require_sha(value)
    if len({expected_base, execution_sha, control_sha, live_base}) != 1:
        raise ValueError("trusted main revision changed or does not match execution")
    actual_number = _field(pr, "number")
    base, head = _field(pr, "base"), _field(pr, "head")
    if (type(actual_number) is not int or actual_number != PR_NUMBER
            or _field(pr, "state") != "open" or _field(pr, "draft") is not True
            or _field(base, "ref") != "main"
            or _field(_field(base, "repo"), "full_name") != REPOSITORY
            or _field(head, "sha") != HEAD
            or _field(_field(head, "repo"), "full_name") != REPOSITORY):
        raise ValueError("source pull request identity changed")
