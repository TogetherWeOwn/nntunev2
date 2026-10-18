"""Unit tests for public_text_lint.

FIXTURE NOTE: this file holds manufactured violations (fake TOG ids,
internal hosts, agent handles) to trial the check. It is excluded from the
lint itself via FIXTURE_EXCLUSIONS in public_text_lint — exact path only,
no directory or extension exemptions.
"""

import contextlib
import io
import os
import sys
import tempfile
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from public_text_lint import (
    FIXTURE_EXCLUSIONS,
    HOST_FIXTURE_EXCLUSIONS,
    _escape_annotation_property,
    find_violations,
    is_artifact_path,
    main,
    scan_path,
)


def rules_for(text, path=""):
    return sorted({rule for _, rule in find_violations(text, path)})


class TestTogIds(unittest.TestCase):
    def test_flags_real_tog_id(self):
        self.assertIn("tog-id", rules_for("see TOG-23644 for details"))

    def test_star_form_is_not_an_id(self):
        self.assertEqual([], find_violations("no `TOG-*` ids in public text"))

    def test_rule_sentence_with_real_id_is_flagged(self):
        # No sentence exemption exists: a real id on a rule line fails
        # like anywhere else. (Manufactured id; this file is excluded
        # from the lint via FIXTURE_EXCLUSIONS.)
        self.assertIn(
            "tog-id",
            rules_for("No tracker IDs (e.g. TOG-00000) are required here"),
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

    def test_dotenv_filename_is_allowed(self):
        self.assertEqual(
            [], find_violations("copy .env.example to .env.local")
        )

    def test_attribute_access_is_not_a_hostname(self):
        self.assertEqual([], find_violations("value = self.internal()"))
        self.assertEqual([], find_violations("value = self.internal.timeout"))

    def test_bare_dotted_name_at_end_still_flags(self):
        # Indistinguishable from a hostname: reword instead of widening.
        self.assertIn("internal-host", rules_for("point it at db.internal"))

    def test_private_suffix_with_path_flags(self):
        self.assertIn("internal-host", rules_for("fetch db.local/rows"))

    def test_sentence_end_punctuation_flags(self):
        # The most common place for a hostname in prose docs: the end of
        # a sentence or wrapped in markup. All of these must flag.
        for text in [
            "The control plane lives at ops.acme.internal.",
            "see <ops.acme.internal> for details",
            "is it ops.acme.internal?",
            "watch out for ops.acme.internal!",
            "config {host: ops.acme.internal}",
            "point at **ops.acme.internal** today",
            "db.internal:3000 is the default",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_chains_and_calls_still_do_not_match(self):
        self.assertEqual([], find_violations("value = self.internal()"))
        self.assertEqual([], find_violations("see foo.internal.bar docs"))
        self.assertEqual([], find_violations("dash is db.internal-legacy"))

    def test_flags_192_168_positives(self):
        for text in [
            "proxy to http://192.168.0.1:8080/x",
            "proxy to http://192.168.1.255:8080/x",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_flags_172_16_12_range_positives(self):
        for text in [
            "host 172.16.0.1 x",
            "host 172.20.5.4 x",
            "host 172.31.255.255 x",
            "host 172.31.255.255!",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_172_15_and_172_32_are_outside_range(self):
        for text in [
            "host 172.15.0.1 x",
            "host 172.15.255.255 x",
            "host 172.32.0.1 x",
            "host 172.32.5.5 x",
        ]:
            self.assertEqual([], find_violations(text), text)

    def test_flags_private_suffix_variants(self):
        for text in [
            "point it at db.lan",
            "point it at db.intranet",
            "point it at db.corp",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_uppercase_suffix_flags(self):
        for text in [
            "point it at DB.INTERNAL",
            "point it at db.LOCAL",
            "point it at DB.LAN:8080",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_paren_lookahead_does_not_match(self):
        # `(` after the host is attribute-call syntax, never a hostname.
        for text in [
            "value = self.internal()",
            "at db.internal(foo)",
            "at ops.acme.internal(foo)",
        ]:
            self.assertEqual([], find_violations(text), text)

    def test_leading_paren_still_flags(self):
        # Only a trailing `(` blocks the match; wrapping parens still flag.
        self.assertIn("internal-host", rules_for("see (db.internal) here"))

    def test_port_suffix_flags(self):
        for text in [
            "db.internal:3000 is the default",
            "point it at db.lan:8080",
            "point it at a.b.corp:443",
        ]:
            self.assertIn("internal-host", rules_for(text), text)

    def test_flags_bare_workforce_host(self):
        # No scheme needed: the workforce control-plane shape flags bare.
        self.assertIn("internal-host", rules_for("bare workforce.host here"))
        self.assertIn("internal-host", rules_for("see workforce.host"))
        self.assertIn("internal-host", rules_for("see WORKFORCE.HOST x"))

    def test_flags_http_scheme_absolute_links(self):
        # `https?` covers `http:` too; absolute links flag internal-host
        # and the embedded relative path flags instance-path.
        rules = rules_for("see http://tracker.example.com/NNTUNE/issues/99 x")
        self.assertIn("internal-host", rules)
        self.assertIn("instance-path", rules)

    def test_absolute_link_flags_all_five_kinds(self):
        for kind in ["issues", "agents", "projects", "approvals", "runs"]:
            text = f"see https://tracker.example.com/NNTUNE/{kind}/1 x"
            self.assertIn("internal-host", rules_for(text), text)

    def test_absolute_link_prefix_length_edges(self):
        for prefix in ["AB", "ABCDEF"]:
            text = f"see https://tracker.example.com/{prefix}/issues/1 x"
            self.assertIn("internal-host", rules_for(text), text)
        for prefix in ["A", "ABCDEFG"]:
            text = f"see https://tracker.example.com/{prefix}/issues/1 x"
            self.assertEqual([], find_violations(text), text)

    def test_absolute_link_requires_trailing_slash(self):
        for text in [
            "see https://tracker.example.com/NNTUNE/issues x",
            "see https://tracker.example.com/NNTUNE/issues99 x",
        ]:
            self.assertNotIn("internal-host", rules_for(text), text)
            self.assertEqual([], find_violations(text), text)

    def test_loopback_and_localhost_are_dev_docs(self):
        self.assertEqual([], find_violations("listen on 127.0.0.1:3000"))
        self.assertEqual([], find_violations("open http://localhost:3000"))


class TestInstancePaths(unittest.TestCase):
    def test_flags_relative_instance_path(self):
        self.assertIn("instance-path", rules_for("see /NNTUNE/issues/99 for context"))

    def test_lowercase_routes_are_code(self):
        self.assertEqual([], find_violations("GET /api/issues/42 returns JSON"))

    def test_flags_all_five_kinds_relative(self):
        for kind in ["issues", "agents", "projects", "approvals", "runs"]:
            text = f"see /NNTUNE/{kind}/1 for context"
            self.assertIn("instance-path", rules_for(text), text)

    def test_relative_prefix_length_edges(self):
        for prefix in ["AB", "ABCDEF"]:
            text = f"see /{prefix}/issues/1 for context"
            self.assertIn("instance-path", rules_for(text), text)
        for prefix in ["A", "ABCDEFG"]:
            text = f"see /{prefix}/issues/1 for context"
            self.assertEqual([], find_violations(text), text)

    def test_relative_requires_trailing_slash(self):
        for text in [
            "see /NNTUNE/issues for context",
            "see /NNTUNE/issues99 for context",
        ]:
            self.assertEqual([], find_violations(text), text)


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

    def test_flags_runner_handle_underscore_variant(self):
        self.assertIn("agent-name", rules_for("run as paperclip_agent today"))

    def test_runner_handle_is_case_insensitive(self):
        for text in [
            "run as PAPERCLIP-AGENT today",
            "run as PAPERCLIP_AGENT today",
            "run as paperclip_agent-7",
        ]:
            self.assertIn("agent-name", rules_for(text), text)

    def test_flags_run_task_session_chat_uuid_vars_with_equals(self):
        for var in [
            "PAPERCLIP_RUN_ID",
            "PAPERCLIP_TASK_ID",
            "PAPERCLIP_SESSION_ID",
            "PAPERCLIP_CHAT_ID",
        ]:
            text = f"{var}=11111111-2222-3333-4444-555555555555"
            self.assertIn("agent-name", rules_for(text), text)

    def test_uuid_vars_accept_colon_separator(self):
        for var in [
            "PAPERCLIP_RUN_ID",
            "PAPERCLIP_TASK_ID",
            "PAPERCLIP_SESSION_ID",
            "PAPERCLIP_CHAT_ID",
        ]:
            text = f"{var}: 11111111-2222-3333-4444-555555555555"
            self.assertIn("agent-name", rules_for(text), text)

    def test_bare_env_names_are_documentable(self):
        self.assertEqual([], find_violations("set PAPERCLIP_API_URL in the environment"))

    def test_role_titles_are_not_agent_names(self):
        self.assertEqual([], find_violations("Owner: CISO"))


class TestCaseSensitivityVerdicts(unittest.TestCase):
    # Pins current behavior for owner confirmation (no pattern change):
    # lowercase tog ids, uppercase AGENT:// and HTTPS:// schemes do not
    # flag their own rule. Flagged in the PR body for owner confirmation
    # that none of these is a hole.
    def test_lowercase_tog_is_not_an_id(self):
        self.assertEqual([], find_violations("see tog-123 here"))
        self.assertIn("tog-id", rules_for("see TOG-123 here"))

    def test_uppercase_agent_scheme_is_not_a_deeplink(self):
        self.assertEqual(
            [], find_violations("ping AGENT://11111111-2222-3333-4444-555555555555")
        )
        self.assertIn(
            "agent-name",
            rules_for("ping agent://11111111-2222-3333-4444-555555555555"),
        )

    def test_uppercase_https_is_not_an_absolute_link(self):
        # The absolute-link rule is lowercase `https?` only, so an
        # uppercase scheme misses internal-host — but the embedded
        # relative UI path still flags instance-path.
        rules = rules_for(
            "see HTTPS://tracker.example.com/NNTUNE/issues/99 x"
        )
        self.assertNotIn("internal-host", rules)
        self.assertIn("instance-path", rules)
        self.assertEqual(
            [], find_violations("see HTTPS://example.com/nothing here")
        )


class TestHostFixtureExclusions(unittest.TestCase):
    # SSRF-guard negative tests must name RFC-reserved ranges to assert
    # rejection. Without a path-scoped exemption those tests are
    # unmergeable; with a whole-file exemption a real leak there would
    # be missed. So: internal-host only, exact paths only.
    SSRF_FIXTURE = '"10.0.0.5",\n"http://169.254.169.254/latest/meta-data/"'

    def test_reserved_ranges_flagged_without_path(self):
        self.assertIn("internal-host", rules_for(self.SSRF_FIXTURE))

    def test_exempt_paths_skip_internal_host_only(self):
        self.assertTrue(HOST_FIXTURE_EXCLUSIONS)  # never an empty bypass
        for path in sorted(HOST_FIXTURE_EXCLUSIONS):
            self.assertNotIn("internal-host", rules_for(self.SSRF_FIXTURE, path))

    def test_exempt_paths_still_flag_tog_ids(self):
        for path in sorted(HOST_FIXTURE_EXCLUSIONS):
            self.assertIn("tog-id", rules_for("see TOG-23644", path))

    def test_exempt_paths_still_flag_other_host_shapes(self):
        # Only the private-IPv4 pattern is skipped on these paths: a
        # pasted hostname, control-plane host or UI link must still fail.
        for path in sorted(HOST_FIXTURE_EXCLUSIONS):
            self.assertIn(
                "internal-host",
                rules_for("dashboard at db.internal:3000", path),
                path,
            )
            self.assertIn(
                "internal-host",
                rules_for("see https://workforce.example.net/abc", path),
                path,
            )
            self.assertIn(
                "internal-host",
                rules_for(
                    "see https://tracker.example.com/NNTUNE/issues/99", path
                ),
                path,
            )

    def test_exempt_paths_still_flag_agent_names(self):
        for path in sorted(HOST_FIXTURE_EXCLUSIONS):
            self.assertIn("agent-name", rules_for("run as paperclip-agent-7", path))

    def test_exclusions_are_files_not_directories(self):
        for path in HOST_FIXTURE_EXCLUSIONS:
            self.assertFalse(path.endswith("/"))


class TestBinaries(unittest.TestCase):
    # A tracked binary IS public text once merged: NUL bytes must not
    # hide content, and compiled artifacts must never be tracked at all
    # (a .pyc once leaked an internal worktree path with CI green).
    NUL_FIXTURE = b"\x00\x01TOG-90000\x00\x02"

    def test_nul_bytes_do_not_hide_content(self):
        with tempfile.TemporaryDirectory() as tmp:
            fixture = os.path.join(tmp, "fixture.bin")
            with open(fixture, "wb") as fh:
                fh.write(self.NUL_FIXTURE)
            self.assertIn((1, "tog-id"), scan_path(fixture))

    def test_artifact_paths_are_rejected_without_reading(self):
        self.assertTrue(is_artifact_path(".github/scripts/__pycache__/x.pyc"))
        self.assertTrue(is_artifact_path("a/b/__pycache__/y.py"))
        self.assertTrue(is_artifact_path("docs/note.pyc"))
        self.assertFalse(is_artifact_path("workers/api/src/session.rs"))
        with tempfile.TemporaryDirectory() as tmp:
            artifact = os.path.join(tmp, "__pycache__", "lint.pyc")
            os.makedirs(os.path.dirname(artifact))
            with open(artifact, "wb") as fh:
                fh.write(b"clean\x00bytes")
            self.assertEqual([(1, "binary-artifact")], scan_path(artifact))


class TestMechanics(unittest.TestCase):
    def test_reports_line_numbers(self):
        lines = find_violations("clean line\nsee TOG-99 here\n")
        self.assertEqual([(2, "tog-id")], lines)

    def test_emoji_before_violation_keeps_line_number(self):
        # The ✅ glyph is bytes E2 9C 85 in UTF-8; scan_path decodes raw
        # bytes as latin-1, so 0x85 becomes U+0085 (NEL), which
        # str.splitlines() treats as a line break. Only the bytes ->
        # latin-1 -> split path pins the split("\n") fix: calling
        # find_violations on a str passes even with splitlines().
        with tempfile.TemporaryDirectory() as tmp:
            fixture = os.path.join(tmp, "emoji.md")
            with open(fixture, "wb") as fh:
                fh.write("✅ ok\nx\nsee TOG-99 here\n".encode("utf-8"))
            self.assertEqual([(3, "tog-id")], scan_path(fixture))

    def test_fixture_path_is_excluded(self):
        self.assertIn(
            ".github/scripts/test_public_text_lint.py", FIXTURE_EXCLUSIONS
        )


class TestMultiRuleHits(unittest.TestCase):
    # One hit per rule per line at most: a line matching two rules
    # yields two hits, the same rule twice on one line yields one.
    def test_one_line_matching_two_rules_yields_two_hits(self):
        hits = find_violations("see TOG-90001 and /NNTUNE/issues/99 here")
        self.assertEqual(2, len(hits))
        self.assertEqual([(1, "instance-path"), (1, "tog-id")], sorted(hits))

    def test_same_rule_twice_on_one_line_yields_one_hit(self):
        self.assertEqual(
            [(1, "tog-id")], find_violations("see TOG-90001 then TOG-90002")
        )

    def test_empty_string_yields_no_hits(self):
        self.assertEqual([], find_violations(""))


class TestArtifactPathEdges(unittest.TestCase):
    # Compiled artifacts are matched on path, never scanned for
    # content. Backslash separators are normalized before matching;
    # forward-slash behavior is unchanged.
    def test_pyo_is_artifact(self):
        self.assertTrue(is_artifact_path("docs/note.pyo"))
        self.assertTrue(is_artifact_path("docs/note.pyc"))

    def test_backslash_paths_are_artifacts(self):
        self.assertTrue(is_artifact_path("docs\\note.pyo"))
        self.assertTrue(is_artifact_path("docs\\note.pyc"))
        self.assertTrue(is_artifact_path("a\\b\\__pycache__\\x.pyc"))

    def test_forward_slash_behavior_unchanged(self):
        self.assertTrue(is_artifact_path(".github/scripts/__pycache__/x.pyc"))
        self.assertTrue(is_artifact_path("a/b/__pycache__/y.py"))
        self.assertFalse(is_artifact_path("workers/api/src/session.rs"))
        self.assertFalse(is_artifact_path("a\\b\\session.rs"))

    def test_pyo_scan_reports_binary_artifact_without_reading(self):
        with tempfile.TemporaryDirectory() as tmp:
            artifact = os.path.join(tmp, "lint.pyo")
            with open(artifact, "wb") as fh:
                fh.write(b"see TOG-90001 here\x00")
            self.assertEqual([(1, "binary-artifact")], scan_path(artifact))


class TestAnnotationEscaping(unittest.TestCase):
    # Workflow-command property values escape %, :, ,, and controls
    # percent-style (% first so existing text stays literal); a
    # violating file with a hostile name still emits the ,line=
    # annotation.
    def test_colon_escapes(self):
        self.assertEqual("a%3Ab", _escape_annotation_property("a:b"))

    def test_percent_and_comma_escape(self):
        self.assertEqual("a%25b", _escape_annotation_property("a%b"))
        self.assertEqual("a%2Cb", _escape_annotation_property("a,b"))
        self.assertEqual("%253A", _escape_annotation_property("%3A"))

    def test_del_and_controls_escape(self):
        self.assertEqual("%7F", _escape_annotation_property("\x7f"))
        self.assertEqual("%00", _escape_annotation_property("\x00"))
        self.assertEqual("a%0Ab", _escape_annotation_property("a\nb"))
        self.assertEqual("a%0Db", _escape_annotation_property("a\rb"))
        self.assertEqual("a%01b", _escape_annotation_property("a\x01b"))
        self.assertEqual("a%1Fb", _escape_annotation_property("a\x1fb"))

    def test_hostile_filename_on_violating_file_emits_line_annotation(self):
        with tempfile.TemporaryDirectory() as tmp:
            hostile = os.path.join(tmp, "evil:a,b%c.md")
            with open(hostile, "w") as fh:
                fh.write("see TOG-90001 here\n")
            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                rc = main(["public_text_lint.py", hostile])
            self.assertEqual(1, rc)
            out = buf.getvalue()
            self.assertIn("evil%3Aa%2Cb%25c.md,line=1", out)
            self.assertIn("tog-id", out)


if __name__ == "__main__":
    unittest.main()
