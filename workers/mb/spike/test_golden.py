#!/usr/bin/env python3
"""Pin golden.py's ports of the ingest query path (workers/ingest/src/matching.rs).

Vectors mirror the ingest unit tests for `derive_upload_query`,
`album_retrieval_query` and `normalize_match_text`, so a drift between the
port and the matcher shows up here. Run: python3 workers/mb/spike/test_golden.py
"""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import golden  # noqa: E402


class GoldenPortTest(unittest.TestCase):
    def test_subject_to_query(self):
        cases = {
            'Artist - Album [01/05] - "01 track.flac" yEnc (1/3)': "Artist - Album",
            'Artist - Album (Live) [01/05] - "x.flac" yEnc': "Artist - Album (Live)",
            "Artist.-.Album [[01/05]] - ": "Artist - Album",
            "Artist_-_Album flac": "Artist - Album",
            '"only a file.flac"': None,
        }
        for subject, expected in cases.items():
            with self.subTest(subject=subject):
                self.assertEqual(golden.derive_upload_query(subject), expected)

    def test_album_side_is_sent(self):
        self.assertEqual(golden.album_retrieval_query("Metallica - Load"), "Load")
        self.assertEqual(golden.album_retrieval_query("Metallica – Load"), "Load")
        self.assertIsNone(golden.album_retrieval_query("Load"))
        self.assertIsNone(golden.album_retrieval_query("Metallica - X"))

    def test_normalize_match_text(self):
        self.assertEqual(golden.normalize_match_text("Rock 'n' Roll"), "rock n roll")
        self.assertEqual(golden.normalize_match_text("Guns N’ Roses"), "guns n roses")
        self.assertEqual(golden.normalize_match_text("AC/DC"), "ac dc")

    def test_variants_reach_the_album_side(self):
        for variant in golden.VARIANTS:
            with self.subTest(variant=variant):
                q = golden.search_q(golden.subject_for("Björk", "Début", variant))
                self.assertIsNotNone(q)
                self.assertEqual(golden.normalize_match_text(golden.ascii_fold(q)), "debut")

    def test_copy_text(self):
        self.assertIsNone(golden.copy_text(r"\N"))
        self.assertEqual(golden.copy_text(r"a\tb\\c"), "a\tb\\c")


if __name__ == "__main__":
    unittest.main()
