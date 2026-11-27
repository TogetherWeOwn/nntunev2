"""Check saved REST protection evidence for the two production database lanes."""

import argparse
import json
import os
import re
import stat
from pathlib import Path

REPOSITORY = "togetherweown/nntunev2"
BOOTSTRAP = "production-db-bootstrap"
BASELINE = "production-db-baseline"
MAX_RESPONSE_BYTES = 256 * 1024


def unique_fields(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError
    return fields


def reject_constant(_):
    raise ValueError


def read_object(path):
    try:
        with open(path, "rb", opener=lambda path, flags: os.open(
            path, flags | os.O_NOFOLLOW | os.O_NONBLOCK,
        )) as source:
            if not stat.S_ISREG(os.fstat(source.fileno()).st_mode):
                return None
            raw = source.read(MAX_RESPONSE_BYTES + 1)
        if len(raw) <= MAX_RESPONSE_BYTES:
            data = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_fields,
                              parse_constant=reject_constant)
            if isinstance(data, dict):
                return data
    except (OSError, ValueError, RecursionError):
        pass
    return None


def field(obj, key):
    return obj.get(key) if isinstance(obj, dict) else None


def approved_reviewer(value):
    reviewer = re.fullmatch(r"(User|Team):([1-9][0-9]{0,19})", value)
    if reviewer is None:
        raise argparse.ArgumentTypeError("use User:ID or Team:ID with a positive numeric GitHub ID")
    return reviewer[1], int(reviewer[2])


def reviewer_rule(rules):
    if not isinstance(rules, list) or any(not isinstance(field(rule, "type"), str) for rule in rules):
        return None
    matches = [rule for rule in rules if rule["type"] == "required_reviewers"]
    return matches[0] if len(matches) == 1 else None


def sole_reviewer_matches(rule, expected):
    reviewers = field(rule, "reviewers")
    if not isinstance(reviewers, list) or len(reviewers) != 1:
        return False
    reviewer = reviewers[0]
    identifier = field(field(reviewer, "reviewer"), "id")
    return type(identifier) is int and (field(reviewer, "type"), identifier) == expected


def environment_issues(name, response, expected_reviewer):
    url = field(response, "url")
    if (field(response, "name") != name or not isinstance(url, str)
            or url.lower() != f"https://api.github.com/repos/{REPOSITORY}/environments/{name}"):
        yield "unresolved: missing or invalid Environment response for the expected repository and name"
        return

    rule = reviewer_rule(response.get("protection_rules"))
    if not sole_reviewer_matches(rule, expected_reviewer):
        yield "refused: the sole required reviewer must match the approved type and ID"
    if field(rule, "prevent_self_review") is not True:
        yield "refused: prevent_self_review must be true on the required reviewers rule"

    bypass = response.get("can_admins_bypass")
    if bypass is True:
        yield "refused: REST reports administrator bypass enabled"
    elif bypass is not False:
        yield ("unresolved: REST did not confirm administrator bypass; the custodian must verify "
               "in this Environment's Settings that administrators cannot bypass protection rules")

    policy = response.get("deployment_branch_policy")
    if field(policy, "protected_branches") is not False or field(policy, "custom_branch_policies") is not True:
        yield "refused: the Environment must use a custom deployment branch policy"


def sole_main_branch(response):
    count = field(response, "total_count")
    policies = field(response, "branch_policies")
    if type(count) is not int or count != 1 or not isinstance(policies, list) or len(policies) != 1:
        return False
    return field(policies[0], "type") == "branch" and field(policies[0], "name") == "main"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence_dir", type=Path, help="directory of saved JSON response bodies")
    parser.add_argument("--bootstrap-reviewer", required=True, type=approved_reviewer)
    parser.add_argument("--baseline-reviewer", required=True, type=approved_reviewer)
    args = parser.parse_args()

    status = 0
    for name, expected in ((BOOTSTRAP, args.bootstrap_reviewer), (BASELINE, args.baseline_reviewer)):
        environment = read_object(args.evidence_dir / f"{name}.environment.json")
        branches = read_object(args.evidence_dir / f"{name}.branch-policies.json")
        issues = list(environment_issues(name, environment, expected))
        if branches is None:
            issues.append("unresolved: missing or invalid deployment branch policy response")
        elif not sole_main_branch(branches):
            issues.append("refused: a complete branch policy response must contain only branch main")
        if issues:
            status = 1
            for issue in issues:
                print(f"{name}: {issue}")
        else:
            print(f"{name}: saved REST protection evidence matches")
    return status


if __name__ == "__main__":
    raise SystemExit(main())
