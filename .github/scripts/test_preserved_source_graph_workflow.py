"""Check the synthetic graph workflow's intended execution boundary."""

import unittest
from pathlib import Path

from test_ci_pnpm_retry import split_jobs, split_steps, step_field


ROOT = Path(__file__).resolve().parents[2]


class TestPreservedSourceGraphWorkflow(unittest.TestCase):
    def test_head_and_graph_control_are_wired(self):
        text = (ROOT / ".github/workflows/preserved-source-graph.yml").read_text()
        self.assertIn("  pull_request:\n", text)
        self.assertNotIn("pull_request_target:", text)
        self.assertIn("permissions:\n  contents: read\n", text)
        jobs = split_jobs(text)
        self.assertEqual({"synthetic-graph"}, set(jobs))
        job = jobs["synthetic-graph"]
        self.assertIn("    timeout-minutes: 5", job)
        steps = split_steps(job)
        self.assertEqual(4, len(steps))
        checkout, identity, wiring, graph = steps
        self.assertEqual("python3 .github/scripts/test_preserved_source_graph_workflow.py -v",
                         step_field(wiring, "run"))
        self.assertEqual("${{ github.event.pull_request.head.sha }}", step_field(checkout, "ref"))
        self.assertEqual("false", step_field(checkout, "persist-credentials"))
        self.assertIn("EXPECTED_HEAD: ${{ github.event.pull_request.head.sha }}", identity)
        self.assertIn('test "$(git rev-parse HEAD)" = "$EXPECTED_HEAD"', identity)
        self.assertIn("git --version", identity)
        self.assertIn("GitFixture.test_preserved_source_requires_original_graph_and_data_objects", graph)
        self.assertNotIn("continue-on-error:", job)
        self.assertNotIn("if:", job)


if __name__ == "__main__":
    unittest.main()
