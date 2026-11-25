"""Exercise saved GitHub REST shapes through the offline command."""

import json
import subprocess
import sys
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

CHECKER = Path(__file__).with_name("database_bootstrap_environment_readback.py")
BOOTSTRAP = "production-db-bootstrap"
BASELINE = "production-db-baseline"


def environment(name, kind, identifier):
    return {
        "name": name,
        "url": f"https://api.github.com/repos/TogetherWeOwn/nntunev2/environments/{name}",
        "can_admins_bypass": False,
        "protection_rules": [{
            "type": "required_reviewers",
            "prevent_self_review": True,
            "reviewers": [{"type": kind, "reviewer": {"id": identifier}}],
        }],
        "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
    }


def branch_policies(**changes):
    return {"total_count": 1, "branch_policies": [{"type": "branch", "name": "main", **changes}]}


class EnvironmentReadbackTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(self.enterContext(TemporaryDirectory(prefix="db-environment-readback-")))
        self.bootstrap = environment(BOOTSTRAP, "User", 101)
        self.baseline = environment(BASELINE, "Team", 202)
        for name, response in ((BOOTSTRAP, self.bootstrap), (BASELINE, self.baseline)):
            self.save(name, "environment", response)
            self.save(name, "branch-policies", branch_policies())

    def save(self, name, response_kind, response):
        (self.directory / f"{name}.{response_kind}.json").write_text(json.dumps(response), encoding="utf-8")

    def check(self, *args):
        return subprocess.run(
            [sys.executable, "-IB", str(CHECKER), str(self.directory), *args],
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=3, check=False,
        )

    def readback(self):
        return self.check("--bootstrap-reviewer", "User:101", "--baseline-reviewer", "Team:202")

    def assert_blocked(self, name, diagnostic):
        result = self.readback()
        self.assertEqual(result.returncode, 1, result)
        self.assertIn(f"{name}: {diagnostic}", result.stdout)
        self.assertNotIn(f"{name}: saved REST protection evidence matches", result.stdout)
        self.assertEqual(result.stderr, "")

    def test_checks_both_exact_environments_with_separately_approved_reviewers(self):
        result = self.readback()
        self.assertEqual(result.returncode, 0, result)
        self.assertEqual(result.stdout, f"{BOOTSTRAP}: saved REST protection evidence matches\n"
                                        f"{BASELINE}: saved REST protection evidence matches\n")
        self.assertEqual(result.stderr, "")

    def test_missing_misidentified_or_unprotected_environment(self):
        (self.directory / f"{BOOTSTRAP}.environment.json").unlink()
        self.assert_blocked(BOOTSTRAP, "unresolved: missing or invalid Environment response")
        for response in ({"message": "Not Found"}, self.baseline,
                         self.bootstrap | {"url": self.bootstrap["url"].replace("nntunev2", "other")}):
            with self.subTest(response=response):
                self.save(BOOTSTRAP, "environment", response)
                self.assert_blocked(BOOTSTRAP, "unresolved: missing or invalid Environment response")
        self.save(BOOTSTRAP, "environment", self.bootstrap | {"protection_rules": [], "deployment_branch_policy": None})
        self.assert_blocked(BOOTSTRAP, "refused: the sole required reviewer")

    def test_rejects_wrong_reviewer_type_or_id_and_additional_reviewers(self):
        rule = self.baseline["protection_rules"][0]
        for reviewers in (
            [{"type": "User", "reviewer": {"id": 202}}],
            [{"type": "Team", "reviewer": {"id": 303}}],
            [{"type": "Team", "reviewer": {"id": "202"}}],
            rule["reviewers"] + [{"type": "User", "reviewer": {"id": 101}}],
            [],
        ):
            with self.subTest(reviewers=reviewers):
                response = self.baseline | {"protection_rules": [rule | {"reviewers": reviewers}]}
                self.save(BASELINE, "environment", response)
                self.assert_blocked(BASELINE, "refused: the sole required reviewer")

    def test_self_review_and_bypass_require_explicit_booleans(self):
        reviewer_rule = self.bootstrap["protection_rules"][0]
        for value in (False, None, 1, "true"):
            with self.subTest(prevent_self_review=value):
                response = self.bootstrap | {"protection_rules": [reviewer_rule | {"prevent_self_review": value}]}
                self.save(BOOTSTRAP, "environment", response)
                self.assert_blocked(BOOTSTRAP, "refused: prevent_self_review")
        self.save(BOOTSTRAP, "environment", self.bootstrap | {"can_admins_bypass": True})
        self.assert_blocked(BOOTSTRAP, "refused: REST reports administrator bypass enabled")
        bootstrap = self.bootstrap.copy()
        del bootstrap["can_admins_bypass"]
        for response in (
            bootstrap, bootstrap | {"can_admins_bypass": None},
            bootstrap | {"can_admins_bypass": 0}, bootstrap | {"can_admins_bypass": "false"},
        ):
            with self.subTest(response=response):
                self.save(BOOTSTRAP, "environment", response)
                self.assert_blocked(BOOTSTRAP, "unresolved: REST did not confirm administrator bypass; "
                                    "the custodian must verify in this Environment's Settings")

    def test_custom_policy_requires_a_complete_sole_typed_main_branch(self):
        path_kind = "branch-policies"
        (self.directory / f"{BASELINE}.{path_kind}.json").unlink()
        self.assert_blocked(BASELINE, "unresolved: missing or invalid deployment branch policy response")
        for response in (
            branch_policies(type="tag"), branch_policies(name="ma*"), branch_policies(name="refs/heads/main"),
            {"total_count": 1, "branch_policies": [{"name": "main"}]},
            {"total_count": 2, "branch_policies": [{"type": "branch", "name": "main"}]},
            {"total_count": 2, "branch_policies": [{"type": "branch", "name": "main"},
                                                  {"type": "tag", "name": "main"}]},
            branch_policies() | {"total_count": True},
            {"branch_policies": [{"type": "branch", "name": "main"}]},
        ):
            with self.subTest(response=response):
                self.save(BASELINE, path_kind, response)
                self.assert_blocked(BASELINE, "refused: a complete branch policy response")
        self.save(BASELINE, path_kind, branch_policies())
        self.save(BASELINE, "environment", self.baseline | {"deployment_branch_policy": {
            "protected_branches": True, "custom_branch_policies": False,
        }})
        self.assert_blocked(BASELINE, "refused: the Environment must use a custom deployment branch policy")

    def test_missing_approval_and_unreadable_evidence_fail_closed(self):
        result = self.check("--bootstrap-reviewer", "User:101")
        self.assertEqual(result.returncode, 2, result)
        self.assertEqual(result.stdout, "")
        path = self.directory / f"{BOOTSTRAP}.environment.json"
        duplicate = json.dumps(self.bootstrap).replace('"can_admins_bypass": false',
                                                       '"can_admins_bypass": true, "can_admins_bypass": false')
        for raw in (b"{", b"\xff", b"[]", duplicate.encode(), b" " * (256 * 1024) + b"{}"):
            with self.subTest(size=len(raw)):
                path.write_bytes(raw)
                self.assert_blocked(BOOTSTRAP, "unresolved: missing or invalid Environment response")


if __name__ == "__main__":
    unittest.main()
