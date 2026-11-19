"""Synthetic tests for offline genre override precedence."""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
spec = importlib.util.spec_from_file_location("overrides", os.path.join(HERE, "overrides.py"))
overrides = importlib.util.module_from_spec(spec)
spec.loader.exec_module(overrides)


class Roots:
    sort_order = {"a": 2, "b": 1, "c": 3}

    def resolve(self, qid):
        return ["a", "c"], True, True


def make(*actions, present=("Q1",)):
    config = {"overrides": [{"genre_qid": "Q1", "action": action, "roots": roots}
                            for action, roots in actions]}
    return overrides.Overrides(config, Roots(), present)


class OverrideTests(unittest.TestCase):
    def test_add_remove_and_deterministic_order(self):
        resolver = make(("add", ["b"]), ("remove", ["c"]))
        self.assertEqual(resolver.resolve("Q1"), (["b", "a"], True, True, False))
        self.assertEqual(resolver.resolve("Q2"), (["a", "c"], True, True, False))

    def test_replace_and_exclude_skip_traversal(self):
        self.assertEqual(make(("replace", ["a", "b"])).resolve("Q1"),
                         (["b", "a"], False, False, False))
        self.assertEqual(make(("exclude", [])).resolve("Q1"), ([], False, False, False))

    def test_missing_target_is_inert(self):
        for action, roots in (("replace", ["b"]), ("exclude", []),
                              ("add", ["b"]), ("remove", ["a"])):
            with self.subTest(action=action):
                self.assertEqual(make((action, roots), present=()).resolve("Q1"),
                                 (["a", "c"], True, True, True))


if __name__ == "__main__":
    unittest.main()
