"""Unit tests for clean_room_lint.

FIXTURE NOTE: this file holds manufactured violations (brand words,
extraction shapes, evidence paths) to trial the check. It is excluded
from the lint itself via DEFINITIONAL_EXCLUSIONS in clean_room_lint —
exact path only, no directory or extension exemptions.
"""

import contextlib
import io
import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from clean_room_lint import find_path_violations, find_violations, main, scan_path


def rules_for(text, path=""):
    return sorted({rule for _, rule in find_violations(text, path)})


class TestExtraction(unittest.TestCase):
    def test_flags_googlevideo(self):
        self.assertIn(
            "youtube-extraction",
            rules_for("stream https://rr1---sn.example.googlevideo.com/videoplayback"),
        )

    def test_flags_youtubei(self):
        self.assertIn(
            "youtube-extraction", rules_for('fetch("/youtubei/v1/player")')
        )

    def test_flags_yt_dlp_forms(self):
        self.assertIn("youtube-extraction", rules_for("run yt-dlp -x"))
        self.assertIn("youtube-extraction", rules_for("import yt_dlp"))

    def test_flags_uppercase(self):
        self.assertIn("youtube-extraction", rules_for("YT-DLP"))

    def test_bare_youtubei_word_does_not_match(self):
        # The rule bans the endpoint shape, not the substring.
        self.assertNotIn("youtube-extraction", rules_for("the youtubei client"))

    def test_official_embed_is_clean(self):
        self.assertEqual(
            [],
            find_violations(
                '<iframe src="https://www.youtube-nocookie.com/embed/abc123">'
            ),
        )


class TestBrand(unittest.TestCase):
    def test_flags_name_any_case(self):
        self.assertIn("competitor-brand", rules_for("import from Spotify"))
        self.assertIn("competitor-brand", rules_for("spotify-green theme"))
        self.assertIn("competitor-brand", rules_for("SPOTIFY"))

    def test_flags_branded_font_name_via_brand_rule(self):
        self.assertIn(
            "competitor-brand", rules_for("font-family: 'Spotify Mix', sans-serif")
        )

    def test_flags_circular_capital(self):
        self.assertIn("competitor-font", rules_for("font-family: Circular"))
        self.assertIn("competitor-font", rules_for("CircularSp book"))
        self.assertIn("competitor-font", rules_for("CircularStd-Medium"))
        self.assertIn("competitor-font", rules_for("CircularTT"))

    def test_lowercase_circular_is_clean(self):
        self.assertEqual([], find_violations("a circular buffer of segments"))

    def test_flags_brand_greens(self):
        self.assertIn("competitor-colour", rules_for("--accent: #1DB954"))
        self.assertIn("competitor-colour", rules_for("color: #1ed760"))
        self.assertIn("competitor-colour", rules_for("0x1DB954"))

    def test_neighbouring_hex_is_clean(self):
        self.assertEqual([], find_violations("--accent: #7c9aff"))


class TestEvidencePath(unittest.TestCase):
    def test_flags_host_research_path(self):
        self.assertIn(
            "host-evidence-path",
            rules_for("evidence in /home/someone/research/notes/"),
        )

    def test_flags_evidence_dir_fragment(self):
        self.assertIn(
            "host-evidence-path", rules_for("see research/nntune-spotify/xpui")
        )

    def test_bare_research_word_is_clean(self):
        self.assertEqual([], find_violations("further research is needed"))


class TestSpikeAllowlist(unittest.TestCase):
    def test_spike_worker_skips_extraction_only(self):
        path = "workers/youtube/src/client.rs"
        self.assertNotIn(
            "youtube-extraction", rules_for("yt-dlp style client", path)
        )
        # Every other rule still applies under the allowlist.
        self.assertIn("competitor-brand", rules_for("Spotify", path))
        self.assertIn("host-evidence-path", rules_for("/home/u/research/x", path))

    def test_spike_doc_skips_extraction(self):
        self.assertNotIn(
            "youtube-extraction",
            rules_for("googlevideo", "docs/youtube-backend-spike.md"),
        )

    def test_similar_paths_are_not_allowlisted(self):
        self.assertIn(
            "youtube-extraction", rules_for("yt-dlp", "workers/youtube2/src/lib.rs")
        )
        self.assertIn(
            "youtube-extraction", rules_for("yt-dlp", "docs/other-spike.md")
        )


class TestPathScan(unittest.TestCase):
    def test_branded_asset_name_flags(self):
        self.assertIn(
            "competitor-brand",
            find_path_violations("apps/web/static/spotify-logo.svg"),
        )

    def test_clean_path(self):
        self.assertEqual(
            [], find_path_violations("apps/web/src/lib/player/queue.ts")
        )

    def test_scan_path_reports_path_rule(self):
        hits = scan_path("AGENTS.md")  # definitional: path-only scan
        self.assertEqual([], hits)

    def test_definitional_files_skip_content_scan(self):
        # AGENTS.md states the ban list and so necessarily names the
        # banned things; its content must not fail the gate.
        self.assertEqual([], scan_path("AGENTS.md"))


class TestMain(unittest.TestCase):
    def test_unreadable_input_fails_closed(self):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = main(["clean_room_lint.py", "no/such/file.txt"])
        self.assertEqual(1, rc)
        self.assertIn("read-error", buf.getvalue())

    def test_report_never_prints_matched_bytes(self):
        # Matched bytes must not reach public CI logs: file:line + rule
        # id only.
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            rc = main(["clean_room_lint.py", "docs/youtube-backend-spike.md"])
        self.assertEqual(0, rc)  # spike doc is allowlisted for extraction
        self.assertNotIn("yt-dlp", buf.getvalue())


if __name__ == "__main__":
    unittest.main()
