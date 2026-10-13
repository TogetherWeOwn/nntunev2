"""Unit tests for public_text_lint.

FIXTURE NOTE: this file holds manufactured violations (fake TOG ids,
internal hosts, agent handles) to trial the check. It is excluded from the
lint itself via FIXTURE_EXCLUSIONS in public_text_lint — exact path only,
no directory or extension exemptions.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from public_text_lint import FIXTURE_EXCLUSIONS, find_violations


def rules_for(text):
    return sorted({rule for _, rule in find_violations(text)})


class TestTogIds(unittest.TestCase):
    def test_flags_real_tog_id(self):
        self.assertIn("tog-id", rules_for("see TOG-23644 for details"))

    def test_star_form_is_not_an_id(self):
        self.assertEqual([], find_violations("no `TOG-*` ids in public text"))

    def test_rule_sentence_is_allowlisted(self):
        self.assertEqual(
            [],
            find_violations("No tracker IDs (e.g. `TOG-*`) are required here"),
        )

    def test_finding_ids_are_public(self):
        self.assertEqual([], find_violations("see F-VAULT-02 in kek-rotation.md"))


class TestInternalHosts(unittest.TestCase):
    def test_flags_dot_internal(self):
        self.assertIn("internal-host", rules_for("dashboard at db.internal:3000"))

    def test_flags_private_ipv4(self):
        self.assertIn("internal-host", rules_for("proxy to http://10.0.3.9:8080/x"))

    def test_flags_metadata_ip(self):
        self.assertIn("internal-host", rules_for("curl 169.254.169.254/latest"))

    def test_flags_workforce_host(self):
        self.assertIn(
            "internal-host", rules_for("see https://workforce.example.net/abc")
        )

    def test_flags_absolute_instance_link(self):
        self.assertIn(
            "internal-host",
            rules_for("see https://tracker.example.com/NNTUNE/issues/99"),
        )

    def test_docker_dev_hostname_is_allowed(self):
        self.assertEqual(
            [], find_violations("connect to host.docker.internal:5432")
        )

    def test_loopback_and_localhost_are_dev_docs(self):
        self.assertEqual([], find_violations("listen on 127.0.0.1:3000"))
        self.assertEqual([], find_violations("open http://localhost:3000"))


class TestInstancePaths(unittest.TestCase):
    def test_flags_relative_instance_path(self):
        self.assertIn("instance-path", rules_for("see /NNTUNE/issues/99 for context"))

    def test_lowercase_routes_are_code(self):
        self.assertEqual([], find_violations("GET /api/issues/42 returns JSON"))


class TestAgentNames(unittest.TestCase):
    def test_flags_agent_deeplink(self):
        self.assertIn(
            "agent-name", rules_for("ping agent://11111111-2222-3333-4444-555555555555")
        )

    def test_flags_runner_handle(self):
        self.assertIn("agent-name", rules_for("run as paperclip-agent-7"))

    def test_flags_uuid_valued_internal_id(self):
        self.assertIn(
            "agent-name",
            rules_for("PAPERCLIP_AGENT_ID=11111111-2222-3333-4444-555555555555"),
        )

    def test_bare_env_names_are_documentable(self):
        self.assertEqual([], find_violations("set PAPERCLIP_API_URL in the environment"))

    def test_role_titles_are_not_agent_names(self):
        self.assertEqual([], find_violations("Owner: CISO"))


class TestMechanics(unittest.TestCase):
    def test_reports_line_numbers(self):
        lines = find_violations("clean line\nsee TOG-99 here\n")
        self.assertEqual([(2, "tog-id")], lines)

    def test_fixture_path_is_excluded(self):
        self.assertIn(
            ".github/scripts/test_public_text_lint.py", FIXTURE_EXCLUSIONS
        )


if __name__ == "__main__":
    unittest.main()
