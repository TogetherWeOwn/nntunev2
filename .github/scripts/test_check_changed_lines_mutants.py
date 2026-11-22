"""Hosted synthetic PR diffs for the changed-lines mutation verdict."""

import json
import os
from pathlib import Path
import sys
from tempfile import TemporaryDirectory
import unittest
from unittest.mock import patch

import check_changed_lines_mutants as gate


SOURCE = "scripts/account_export.py"
OUTSIDE = "scripts/unmapped.py"
TEST = "scripts/test_account_export.py"
FIXTURE = json.loads((Path(__file__).parent / "fixtures/changed_lines_mutation.json").read_bytes())


class ChangedLinesTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(TemporaryDirectory()))
        self.git("init", "-q")
        self.base = self.commit({SOURCE: FIXTURE["base"], OUTSIDE: FIXTURE["base"]})

    def git(self, *args):
        return gate.git(self.root, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", *args)

    def commit(self, files):
        for path, content in files.items():
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(content, encoding="utf-8")
        self.git("add", ".")
        self.git("commit", "-qm", "fixture snapshot")
        return self.git("rev-parse", "HEAD").strip().decode()

    def test_hosted_verdicts(self):
        for case in FIXTURE["cases"]:
            with self.subTest(case=case["name"]):
                source = FIXTURE["head"]
                if "implementation" in case:
                    source = source.replace("return value < 8", case["implementation"])
                test = FIXTURE["test_template"].format(assertion=case["assertion"])
                merge = self.commit({SOURCE: source, OUTSIDE: FIXTURE["head"], TEST: test})
                evidence = []

                def evaluate():
                    return gate.evaluate(self.root, self.base, merge, case["enforcing"],
                                         report=evidence.append, warn=evidence.append)

                if "error" in case:
                    with self.assertRaisesRegex(gate.GateError, case["error"]):
                        evaluate()
                else:
                    self.assertEqual(evaluate(), case["verdict"])
                    for observation in case["evidence"]:
                        self.assertIn(observation, "\n".join(evidence))
                self.assertEqual((self.root / SOURCE).read_text(encoding="utf-8"), source)
                print(f"Fixture passed: {case['name']}")

    def test_rollout_uses_main_anchor_and_enforces_at_one_week(self):
        tested = self.commit({OUTSIDE: "changed away from main"})
        self.assertFalse(gate.rollout(None, 0)[0])
        with patch.dict(os.environ, GIT_COMMITTER_DATE="2000-01-01T00:00:00Z"):
            anchor = self.commit({gate.ROLLOUT_PATH: "present on main"})
        start = int(self.git("show", "-s", "--format=%ct", anchor))
        self.git("update-ref", gate.MAIN_REF, anchor)
        self.git("checkout", "--detach", tested)
        self.assertFalse(gate.rollout(start, start + gate.WEEK - 1)[0])
        self.assertTrue(gate.rollout(start, start + gate.WEEK)[0])
        summary = self.root / "summary"
        with patch.object(Path, "cwd", return_value=self.root), patch.dict(
            os.environ, GITHUB_BASE_REF="preview", MUTATION_BASE_SHA=self.base,
            MUTATION_MERGE_SHA=tested, GITHUB_STEP_SUMMARY=str(summary),
        ):
            self.assertEqual(gate.main(), 0)
            with patch.dict(os.environ, MUTATION_BASE_SHA=anchor), self.assertRaisesRegex(gate.GateError, "first parent"):
                gate.main()
        self.assertIn("Enforcement phase", summary.read_text(encoding="utf-8"))


if __name__ == "__main__":
    if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("GITHUB_EVENT_NAME") != "pull_request":
        sys.exit("Mutation fixtures run only on GitHub Actions pull requests")
    unittest.main(verbosity=2)
