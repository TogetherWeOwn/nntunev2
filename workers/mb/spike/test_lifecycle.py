#!/usr/bin/env python3
"""Pin lifecycle.py's expiry-rule check: name, prefix, 14 days and enabled
must hold on one rule.

Run: python3 workers/mb/spike/test_lifecycle.py
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import lifecycle  # noqa: E402


def rule(**overrides) -> dict:
    out = lifecycle.desired_rule()
    out.update(overrides)
    return out


class RuleOk(unittest.TestCase):
    def test_desired_rule_passes(self):
        self.assertTrue(lifecycle.rule_ok({"rules": [rule()]}))

    def test_empty_or_missing_fails(self):
        self.assertFalse(lifecycle.rule_ok({}))
        self.assertFalse(lifecycle.rule_ok({"rules": []}))

    def test_other_expiry_fails(self):
        thirty = {"condition": {"type": "Age", "maxAge": 30 * 86400}}
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(deleteObjectsTransition=thirty)]}))

    def test_date_expiry_fails(self):
        dated = {"condition": {"type": "Date", "date": "2027-01-01T00:00:00Z"}}
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(deleteObjectsTransition=dated)]}))

    def test_no_expiry_action_fails(self):
        bare = rule()
        del bare["deleteObjectsTransition"]
        self.assertFalse(lifecycle.rule_ok({"rules": [bare]}))

    def test_disabled_fails(self):
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(enabled=False)]}))

    def test_other_prefix_fails(self):
        self.assertFalse(lifecycle.rule_ok({"rules": [rule(conditions={"prefix": "other/"})]}))

    def test_name_and_prefix_split_across_rules_fails(self):
        split = [
            rule(conditions={"prefix": "other/"}),
            rule(id="something-else"),
        ]
        self.assertFalse(lifecycle.rule_ok({"rules": split}))


class Repaired(unittest.TestCase):
    def test_replaces_stale_rule_and_keeps_others(self):
        other = {"id": "abort-multipart", "enabled": True, "conditions": {"prefix": ""}}
        stale = rule(enabled=False)
        fixed = lifecycle.repaired({"rules": [other, stale]})
        self.assertEqual(fixed["rules"], [other, lifecycle.desired_rule()])
        self.assertTrue(lifecycle.rule_ok(fixed))


if __name__ == "__main__":
    unittest.main()
