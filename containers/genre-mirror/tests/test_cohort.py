#!/usr/bin/env python3
"""Synthetic cohort admission contract tests."""

import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import cohort

A = "12345678-1234-1234-1234-123456789abc"
B = "abcdefab-abcd-abcd-abcd-abcdefabcdef"


def fixture(**changes):
    value = {"schema": "nntune.genre-artists.v1", "dataset_kind": "synthetic",
             "artist_mbids": [A, B], "playable_artist_mbids": [A]}
    value.update(changes)
    return json.dumps(value).encode()


class CohortTests(unittest.TestCase):
    def test_valid_synthetic_and_normalization(self):
        result = cohort.admit_cohort(fixture(artist_mbids=[B.upper(), A],
                                              playable_artist_mbids=[B.upper()]))
        self.assertEqual(result["artist_mbids"], [A, B])
        self.assertEqual(result["playable_artist_mbids"], [B])
        self.assertEqual(cohort.admit_cohort(fixture(artist_mbids=[], playable_artist_mbids=[]))
                         ["artist_mbids"], [])

    def test_rejects_bad_json(self):
        for raw in (b"", b"{} trailing", b'\xff', b'{"x":NaN}',
                    b'{"schema":1,"schema":2}', b"[" * 1200 + b"]" * 1200):
            with self.subTest(raw=raw[:40]), self.assertRaises(ValueError):
                cohort.admit_cohort(raw)

    def test_rejects_duplicate_json_key_in_valid_cohort(self):
        raw = fixture().replace(b'"dataset_kind": "synthetic"',
                                b'"dataset_kind": "synthetic", "dataset_kind": "synthetic"')
        with self.assertRaisesRegex(ValueError, "duplicate"):
            cohort.admit_cohort(raw)

    def test_rejects_bad_declarations_and_shapes(self):
        for raw in (fixture(dataset_kind="actual-export"), fixture(dataset_kind="other"),
                    fixture(schema="other"), fixture(extra=True), b"[]", b"{}",
                    fixture(artist_mbids={}), fixture(playable_artist_mbids=None)):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                cohort.admit_cohort(raw)

    def test_rejects_invalid_and_nil_uuids(self):
        for value in (None, 1, "", "12345678123412341234123456789abc",
                      "00000000-0000-0000-0000-000000000000", A + "x"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                cohort.admit_cohort(fixture(artist_mbids=[value]))

    def test_rejects_duplicates_and_missing_playable(self):
        for raw in (fixture(artist_mbids=[A, A.upper()]),
                    fixture(playable_artist_mbids=[A, A]),
                    fixture(artist_mbids=[A], playable_artist_mbids=[B])):
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                cohort.admit_cohort(raw)

    def test_byte_and_row_bounds(self):
        raw = fixture()
        with patch.object(cohort, "MAX_BYTES", len(raw)):
            cohort.admit_cohort(raw)
            with self.assertRaises(ValueError):
                cohort.admit_cohort(raw + b" ")
        with patch.object(cohort, "MAX_ROWS", 1):
            cohort.admit_cohort(fixture(artist_mbids=[A], playable_artist_mbids=[A]))
            with self.assertRaises(ValueError):
                cohort.admit_cohort(fixture())
            with self.assertRaises(ValueError):
                cohort.admit_cohort(fixture(artist_mbids=[A], playable_artist_mbids=[A, A]))
        with self.assertRaises(ValueError):
            cohort.admit_cohort("not bytes")


if __name__ == "__main__":
    unittest.main()
