"""Synthetic offline ancestry traversal tests."""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "genre_mirror_graph", os.path.join(os.path.dirname(HERE), "graph.py")
)
graph = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(graph)


class GraphTests(unittest.TestCase):
    def reach(self, parents, start="Q1", barriers=(), depth=4, nodes=20):
        return graph.Graph(parents, frozenset(barriers), depth, nodes).reach(start)

    def test_leaf_and_diamond(self):
        self.assertEqual(self.reach({}, "Q9"), (frozenset({"Q9"}), False, False))
        parents = {"Q1": ["Q2", "Q3"], "Q2": ["Q4"], "Q3": ["Q4"]}
        self.assertEqual(
            self.reach(parents), (frozenset({"Q1", "Q2", "Q3", "Q4"}), False, False)
        )

    def test_cycle_and_self_loop(self):
        self.assertEqual(
            self.reach({"Q1": ["Q2"], "Q2": ["Q1", "Q3"]}),
            (frozenset({"Q1", "Q2", "Q3"}), False, True),
        )
        self.assertEqual(self.reach({"Q1": ["Q1"]}), (frozenset({"Q1"}), False, True))

    def test_barriers_are_not_visited_or_capped(self):
        parents = {"Q1": ["Q2"], "Q2": ["Q3"]}
        self.assertEqual(self.reach(parents, barriers=("Q1",)), (frozenset(), False, False))
        self.assertEqual(self.reach(parents, barriers=("Q2",)), (frozenset({"Q1"}), False, False))

    def test_depth_cap_preserves_reached_nodes_and_exact_boundary(self):
        parents = {"Q1": ["Q2", "Q3"], "Q2": ["Q4"], "Q4": ["Q5"]}
        self.assertEqual(
            self.reach(parents, depth=2), (frozenset({"Q1", "Q2", "Q3", "Q4"}), True, False)
        )
        self.assertEqual(
            self.reach(parents, start="Q2", depth=2),
            (frozenset({"Q2", "Q4", "Q5"}), False, False),
        )

    def test_node_cap_and_cycle_within_visited_closure(self):
        parents = {"Q1": ["Q2", "Q3"], "Q2": ["Q1"]}
        self.assertEqual(self.reach(parents, nodes=2), (frozenset({"Q1", "Q2"}), True, True))
        self.assertEqual(
            self.reach(parents, nodes=3), (frozenset({"Q1", "Q2", "Q3"}), False, True)
        )


if __name__ == "__main__":
    unittest.main()
