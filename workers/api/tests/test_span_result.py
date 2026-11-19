"""Offline contract tests for the span observation evaluator."""

import copy
import unittest

from span_result import evaluate_negative, evaluate_positive


def scope(identity):
    return {"before": identity, "mid": identity, "end": identity, "after": "root"}


def positive():
    return {"root": "root", "tasks": [scope("one"), scope("two")],
            "child": scope("child")}


def negative():
    return {key: "root" for key in ("root", "before", "mid", "end", "after")}


class SpanResultTests(unittest.TestCase):
    def test_valid_observations(self):
        evaluate_positive(positive())
        evaluate_negative(negative())

    def test_each_scope_checkpoint_is_checked(self):
        for item in (0, 1, "child"):
            for key in ("before", "mid", "end", "after"):
                with self.subTest(item=item, key=key):
                    value = positive()
                    target = value["child"] if item == "child" else value["tasks"][item]
                    target[key] = "wrong"
                    with self.assertRaises(ValueError):
                        evaluate_positive(value)

    def test_root_and_scoped_identities_are_distinct(self):
        for index, duplicate in ((0, "root"), (1, "one"), ("child", "one")):
            value = positive()
            if index == "child":
                value["child"] = scope(duplicate)
            else:
                value["tasks"][index] = scope(duplicate)
            with self.subTest(index=index), self.assertRaises(ValueError):
                evaluate_positive(value)

    def test_positive_shape_and_invalid_identities_fail_closed(self):
        cases = [None, {}, {**positive(), "extra": True},
                 {**positive(), "tasks": []}, {**positive(), "tasks": [scope("one")]},
                 {**positive(), "tasks": "not a list"},
                 {**positive(), "root": ""}, {**positive(), "root": None},
                 {**positive(), "child": {}},
                 {**positive(), "child": {**scope("child"), "extra": "x"}}]
        for value in cases:
            with self.subTest(value=value), self.assertRaises(ValueError):
                evaluate_positive(value)
        for invalid in (None, "", " ", 1, True, [], {}):
            value = positive()
            value["tasks"][0]["mid"] = invalid
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                evaluate_positive(value)

    def test_negative_checks_every_root_identity(self):
        for key in negative():
            for invalid in ("wrong", "", None, 0, True):
                value = negative()
                value[key] = invalid
                with self.subTest(key=key, invalid=invalid), self.assertRaises(ValueError):
                    evaluate_negative(value)

    def test_negative_rejects_missing_extra_and_arbitrary_failures(self):
        for key in negative():
            value = negative()
            del value[key]
            with self.subTest(missing=key), self.assertRaises(ValueError):
                evaluate_negative(value)
        for value in (None, False, [], {"error": "failed"},
                      {**negative(), "error": "failed"}, copy.deepcopy(positive())):
            with self.subTest(value=value), self.assertRaises(ValueError):
                evaluate_negative(value)


if __name__ == "__main__":
    unittest.main()
