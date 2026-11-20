#!/usr/bin/env python3
"""Pin golden.py's ports of the ingest query path (workers/ingest/src/matching.rs).

Vectors mirror the ingest unit tests for `derive_upload_query`,
`album_retrieval_query` and `normalize_match_text`, so a drift between the
port and the matcher shows up here. Run: python3 workers/mb/spike/test_golden.py
"""

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

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


class GoldenSamplingTest(unittest.TestCase):
    def test_heldout_excludes_all_three_inspected_seeds_by_query_group(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "mbdump-artist.tsv").write_text("artist\tArtist\n")
            # Repeated titles across releases must not leak between samples.
            (root / "mbdump-release.tsv").write_text("".join(
                f"{i}\tartist\tAlbum {i // 3}\n" for i in range(180)
            ))
            def generate(name, seed, *excludes):
                out = root / name
                args = ["golden.py", str(root), str(out), "12", str(seed)]
                for path in excludes:
                    args += ["--exclude", str(path)]
                with patch.object(sys, "argv", args), contextlib.redirect_stderr(io.StringIO()):
                    self.assertEqual(golden.main(), 0)
                return out
            first = generate("tuning.tsv", 3599)
            second = generate("tuning-mark-fold.tsv", 20261107)
            third = generate("inspected-heldout.tsv", 20261108)
            heldout = generate("heldout.tsv", 20261109, first, second, third)
            keys = lambda path: {golden.query_group(r.split("\t")[1])
                                 for r in path.read_text().splitlines()}
            self.assertEqual(len(heldout.read_text().splitlines()), 4)  # rare stratum only
            self.assertFalse(keys(heldout) & (keys(first) | keys(second) | keys(third)))
            again = generate("again.tsv", 20261109, first, second, third)
            self.assertEqual(heldout.read_bytes(), again.read_bytes())

    def test_query_group_collapses_variant_spelling(self):
        self.assertEqual(golden.query_group("DÉBUT (Live)"), golden.query_group("debut live"))

    def test_negative_count_is_refused_before_reading_dump(self):
        with patch.object(sys, "argv", ["golden.py", "/missing", "/unused", "-1000"]):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                golden.main()
        self.assertEqual(error.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
