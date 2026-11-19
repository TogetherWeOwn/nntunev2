"""Tests for the fixed graph qualification context."""

import unittest

import graph_qualification_context as context


BASE = "a" * 40


def pull_request():
    return {
        "number": context.PR_NUMBER, "state": "open", "draft": True,
        "base": {"ref": "main", "repo": {"full_name": context.REPOSITORY}},
        "head": {"sha": context.HEAD, "repo": {"full_name": context.REPOSITORY}},
    }


class ContextTests(unittest.TestCase):
    def validate(self, pr=None, number="1072", expected=BASE, execution=BASE,
                 controls=BASE, live=BASE):
        context.validate_context(number, expected, execution, controls, live,
                                 pull_request() if pr is None else pr)

    def test_valid_context(self):
        self.validate()

    def test_rejects_bad_scope_and_revisions(self):
        for change in (
            {"number": "1073"}, {"number": "1072;false"},
            {"expected": "main"}, {"expected": "A" * 40},
            {"execution": "d" * 40}, {"controls": "d" * 40}, {"live": "d" * 40},
        ):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.validate(**change)

    def test_rejects_changed_pr_metadata(self):
        changes = (
            ("number", 1073), ("state", "closed"), ("draft", False),
            ("base.ref", "other"), ("base.repo.full_name", "other/repo"),
            ("head.sha", "d" * 40), ("head.repo.full_name", "other/repo"),
            ("head.repo", None),
        )
        for path, value in changes:
            pr = pull_request()
            parts = path.split(".")
            node = pr
            for part in parts[:-1]:
                node = node[part]
            node[parts[-1]] = value
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.validate(pr=pr)


if __name__ == "__main__":
    unittest.main()
