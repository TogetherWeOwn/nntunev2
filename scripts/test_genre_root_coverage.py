"""Synthetic-only coverage and CLI contract tests; run in GitHub Actions."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import genre_root_coverage as coverage

FIXTURES = Path(__file__).with_name("fixtures") / "genre_root_coverage"


def mbid(number):
    return f"aaaaaaaa-0000-4000-8000-{number:012x}"


def document(covered=4, total=5):
    return {
        "schema_version": 1,
        "dataset_kind": "synthetic",
        "playable_artist_mbids": [mbid(i) for i in range(total)],
        "resolved_root_ids": ["root-a", "root-b"],
        "memberships": [{"artist_mbid": mbid(i), "root_id": "root-a"}
                        for i in range(covered)],
    }


class CoverageTests(unittest.TestCase):
    def test_synthetic_fixture(self):
        result = coverage.calculate(coverage.load(FIXTURES / "synthetic.json"))
        self.assertEqual(result, coverage.load(FIXTURES / "synthetic.expected.json"))

    def test_duplicates_multiroot_unknown_and_nonplayable(self):
        data = document()
        data["playable_artist_mbids"] += [mbid(0), mbid(0).upper()]
        data["resolved_root_ids"] += ["root-a"]
        data["memberships"] += [data["memberships"][0],
                                {"artist_mbid": mbid(0).upper(), "root_id": "root-b"},
                                {"artist_mbid": mbid(4), "root_id": "unknown"},
                                {"artist_mbid": mbid(4), "root_id": None},
                                {"artist_mbid": mbid(99), "root_id": "root-a"},
                                {"artist_mbid": mbid(99), "root_id": "ignored"}]
        result = coverage.calculate(data)
        self.assertEqual((result["numerator"], result["denominator"],
                          result["uncovered_artists"]), (4, 5, 1))
        self.assertEqual(result["unknown_root_ids_for_playable_artists"], ["unknown"])
        for field in ("playable_artist_mbids", "resolved_root_ids", "memberships"):
            data[field].reverse()
        self.assertEqual(result, coverage.calculate(data))

    def test_threshold_boundaries_and_exact_percentage(self):
        for count, total, met, exact, display in [
            (0, 5, False, "0", "0.000000"), (3, 5, False, "60", "60.000000"),
            (4, 5, True, "80", "80.000000"), (5, 5, True, "100", "100.000000"),
            (2, 3, False, "200/3", "66.666667"),
        ]:
            with self.subTest(count=count, total=total):
                result = coverage.calculate(document(count, total))
                self.assertEqual(result["threshold_met"], met)
                self.assertEqual(result["percentage_exact"], exact)
                self.assertEqual(result["percentage_display"], display)
        # Immediately below, at and above the fixed threshold.
        for count, met in [(79, False), (80, True), (81, True)]:
            self.assertEqual(coverage.calculate(document(count, 100))["threshold_met"], met)

    def test_empty_denominator_and_no_resolved_roots(self):
        result = coverage.calculate(document(1, 0))
        self.assertFalse(result["threshold_met"])
        self.assertIsNone(result["percentage_exact"])
        self.assertIsNone(result["percentage_display"])
        self.assertEqual(result["status"], "no_playable_artists")
        data = document()
        data["resolved_root_ids"] = []
        self.assertEqual(coverage.calculate(data)["numerator"], 0)

    def test_actual_export_label_is_not_gate_attestation(self):
        data = document()
        data["dataset_kind"] = "actual-export"
        result = coverage.calculate(data)
        self.assertTrue(result["threshold_met"])
        self.assertEqual(result["real_data_gate"], "not_evaluated")
        self.assertEqual(result["dataset_kind"], "actual-export")

    def test_malformed_contract(self):
        mutations = [
            ("schema_version", True), ("schema_version", 2), ("schema_version", 1.0),
            ("dataset_kind", "proxy"), ("dataset_kind", []),
            ("playable_artist_mbids", "not-an-array"),
            ("playable_artist_mbids", ["bad"]), ("playable_artist_mbids", [None]),
            ("playable_artist_mbids", ["00000000-0000-0000-0000-000000000000"]),
            ("resolved_root_ids", [" Root"]), ("resolved_root_ids", ["røot"]),
            ("resolved_root_ids", [None]), ("resolved_root_ids", ["x" * 65]),
            ("memberships", [None]), ("memberships", [{"artist_mbid": mbid(0)}]),
            ("memberships", [{"artist_mbid": mbid(99), "root_id": 1}]),
        ]
        for field, value in mutations:
            with self.subTest(field=field, value=value):
                data = document()
                data[field] = value
                with self.assertRaises(coverage.InputError):
                    coverage.calculate(data)
        for data in [None, [], {}, dict(document(), extra=1)]:
            with self.assertRaises(coverage.InputError):
                coverage.calculate(data)

    def test_cli_output_and_exit_codes(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.json"
            for data, code in [(document(), 0), (document(3), 1), (document(0, 0), 1)]:
                path.write_text(json.dumps(data), encoding="utf-8")
                first = self.cli(path)
                self.assertEqual(first.returncode, code, first.stderr)
                self.assertEqual(first.stderr, "")
                self.assertEqual(first.stdout, self.cli(path).stdout)
                self.assertEqual(json.loads(first.stdout), coverage.calculate(data))
            for raw in [b"{", b"\xff", b'{"x":1,"x":2}', b"NaN", b"Infinity",
                        b"[" * 2000 + b"]" * 2000, b"null"]:
                path.write_bytes(raw)
                result = self.cli(path)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
                self.assertIn("coverage input error:", result.stderr)
            path.unlink()
            self.assertEqual(self.cli(path).returncode, 2)

    def test_size_bound(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.json"
            path.write_bytes(b" " * 11)
            with patch.object(coverage, "MAX_INPUT_BYTES", 10):
                with self.assertRaises(coverage.InputError):
                    coverage.load(path)

    @staticmethod
    def cli(path):
        return subprocess.run([sys.executable, str(Path(coverage.__file__)), str(path)],
                              text=True, capture_output=True, timeout=10)


if __name__ == "__main__":
    unittest.main()
