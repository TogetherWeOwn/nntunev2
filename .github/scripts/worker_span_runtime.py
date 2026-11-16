#!/usr/bin/env python3
"""Evaluate Worker span probe observations with offline self-tests."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[2] / "workers/api/tests"))
from span_result import evaluate_positive, evaluate_negative as evaluate_root_only

TASKS, REQUESTS = 8, 6


def check(cond, message):
    if not cond:
        raise ValueError(message)


def observations(body, mode):
    check(isinstance(body, dict) and body.keys() == {"mode", "root_after", "child_expected", "overlapping", "observations"}
          and body["mode"] == mode and type(body["overlapping"]) is bool, "bad response envelope")
    value = body["observations"]
    check(isinstance(value, dict) and value.keys() == {"root", "tasks", "child"},
          "bad observations")
    check(isinstance(value["tasks"], list) and len(value["tasks"]) == TASKS,
          "wrong task count")
    check(body["root_after"] == value["root"], "root span not restored after join")
    return value


def evaluate_scoped(bodies):
    ids = []
    for body in bodies:
        value = observations(body, "scoped")
        evaluate_positive(value)
        check(value["child"]["before"] == body["child_expected"], "captured child span mismatch")
        ids.append(value["root"])
        ids.extend(item["before"] for item in [*value["tasks"], value["child"]])
    check(len(set(ids)) == len(ids), "span ids collided across concurrent requests")


def evaluate_burst(bodies):
    evaluate_scoped(bodies)
    check(any(body["overlapping"] for body in bodies), "requests did not overlap")


def evaluate_negative(body):
    value = observations(body, "unscoped")
    for item in [*value["tasks"], value["child"]]:
        check(isinstance(item, dict) and item.keys() == {"before", "mid", "end", "after"},
              "malformed control observation")
        evaluate_root_only({"root": value["root"], **item})
    try:
        evaluate_positive(value)
    except ValueError:
        return
    raise ValueError("positive evaluator accepted the unscoped control")


class SelfTest(unittest.TestCase):
    def body(self, scoped):
        def sample(identity):
            return {"before": identity, "mid": identity, "end": identity, "after": "root"}
        return {"mode": "scoped" if scoped else "unscoped", "root_after": "root",
                "child_expected": "child" if scoped else None, "overlapping": False, "observations": {"root": "root",
                                 "tasks": [sample(f"task-{i}" if scoped else "root")
                                           for i in range(TASKS)],
                                 "child": sample("child" if scoped else "root")}}

    def test_negative_control(self):
        evaluate_negative(self.body(False))
        with self.assertRaises(ValueError):
            evaluate_scoped([{**self.body(False), "mode": "scoped"}])
        with self.assertRaises(ValueError):
            evaluate_negative(self.body(True))
        body = self.body(False)
        body["observations"]["tasks"][0] = dict.fromkeys(
            ("root", "before", "mid", "end", "after"), "other")
        with self.assertRaisesRegex(ValueError, "malformed control observation"):
            evaluate_negative(body)
        body = self.body(False)
        body["observations"]["tasks"][0]["before"] = "other"
        with self.assertRaisesRegex(ValueError, "negative control did not observe root throughout"):
            evaluate_negative(body)

    def test_concurrent_collision(self):
        body = self.body(True)
        evaluate_scoped([body])
        with self.assertRaisesRegex(ValueError, "requests did not overlap"):
            evaluate_burst([body])
        body["overlapping"] = True
        evaluate_burst([body])
        other = self.body(True)
        for item in [*other["observations"]["tasks"], other["observations"]["child"]]:
            for key in ("before", "mid", "end"):
                item[key] += "-other"
        other["child_expected"] += "-other"
        with self.assertRaisesRegex(ValueError, "collided"):
            evaluate_scoped([body, other])
        body["child_expected"] = "different-captured-span"
        with self.assertRaisesRegex(ValueError, "captured child span mismatch"):
            evaluate_scoped([body])


if __name__ == "__main__":
    if sys.argv[1:] != ["--self-test"]:
        raise SystemExit("usage: worker_span_runtime.py --self-test")
    unittest.main(argv=[sys.argv[0]])
