"""Synthetic tests for offline root selection."""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def load(name):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, name + ".py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


graph = load("graph")
roots = load("roots")


def root(rid, order, *anchors, retired=False):
    return {"root_id": rid, "sort_order": order, "anchors": list(anchors), "retired": retired}


def resolver(parents, root_list, barriers=(), depth=10, nodes=20):
    traversal = graph.Graph(parents, frozenset(barriers), depth, nodes)
    return roots.Roots({"roots": root_list}, traversal)


class RootTests(unittest.TestCase):
    def test_specific_roots_and_sort_order(self):
        r = resolver({"Q2": ["Q1"], "Q9": ["Q2", "Q3"]},
                     [root("broad", 2, "Q1"), root("narrow", 3, "Q2"),
                      root("other", 1, "Q3"), root("old", 4, "Q9", retired=True)])
        self.assertEqual(r.resolve("Q9"), (["other", "narrow"], False, False))
        self.assertEqual(r.resolve("Q1"), (["broad"], False, False))
        self.assertEqual(r.resolve("Q8"), ([], False, False))

    def test_mutual_cycle_keeps_both_and_reports_cycle(self):
        r = resolver({"Q1": ["Q2"], "Q2": ["Q1"], "Q3": ["Q1"]},
                     [root("a", 1, "Q1"), root("b", 2, "Q2")])
        self.assertEqual(r.resolve("Q3"), (["a", "b"], False, True))

    def test_projected_cycle_from_multiple_anchors_keeps_all(self):
        parents = {"Q1": ["Q3"], "Q4": ["Q5"], "Q6": ["Q2"],
                   "Q7": ["Q1", "Q4", "Q6"]}
        r = resolver(parents, [root("a", 1, "Q1", "Q2"), root("b", 2, "Q3", "Q4"),
                               root("c", 3, "Q5", "Q6")])
        self.assertEqual(r.resolve("Q7"), (["a", "b", "c"], False, False))

    def test_projected_cycle_containing_mutual_pair_keeps_all(self):
        parents = {"Q1": ["Q3"], "Q4": ["Q2", "Q5"], "Q6": ["Q2"],
                   "Q9": ["Q1", "Q4", "Q6"]}
        r = resolver(parents, [root("a", 1, "Q1", "Q2"), root("b", 2, "Q3", "Q4"),
                               root("c", 3, "Q5", "Q6")])
        self.assertEqual(r.resolve("Q9"), (["a", "b", "c"], False, False))

    def test_capped_anchor_precomputation_does_not_prove_dominance(self):
        parents = {"Q1": ["Q2"], "Q2": ["Q3"], "Q3": ["Q1"],
                   "Q9": ["Q1", "Q2", "Q3"]}
        r = resolver(parents, [root("a", 1, "Q1"), root("b", 2, "Q2")], depth=1)
        self.assertEqual(r.resolve("Q9"), (["a", "b"], True, True))

    def test_barrier_and_cap_preserve_only_reached_roots(self):
        parents = {"Q9": ["Q1", "Q2"], "Q2": ["Q3"]}
        configured = [root("a", 1, "Q1"), root("b", 2, "Q3")]
        self.assertEqual(resolver(parents, configured, barriers=("Q2",)).resolve("Q9"),
                         (["a"], False, False))
        self.assertEqual(resolver(parents, configured, depth=1).resolve("Q9"),
                         (["a"], True, False))
        self.assertEqual(resolver(parents, configured, nodes=1).resolve("Q9"),
                         ([], True, False))


if __name__ == "__main__":
    unittest.main()
