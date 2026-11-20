"""Synthetic integration tests for the offline resolver chain."""

import json
import os
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from rollup import resolve
import synthetic_resolver


def fixture():
    config = {
        "taxonomy_version": 1,
        "traversal_barriers": [],
        "limits": {"max_depth": 8, "max_nodes": 20, "broad_multi_threshold": 1},
        "roots": [
            {"root_id": "broad", "display_name": "Broad", "sort_order": 2, "anchors": ["Q1"]},
            {"root_id": "narrow", "display_name": "Narrow", "sort_order": 1, "anchors": ["Q2"]},
        ],
        "overrides": [
            {"genre_qid": "Q4", "action": "add", "roots": ["broad"], "reason": "fixture"},
            {"genre_qid": "Q5", "action": "replace", "roots": ["broad"], "reason": "fixture"},
            {"genre_qid": "Q9", "action": "replace", "roots": ["narrow"], "reason": "fixture"},
            {"genre_qid": "Q10", "action": "exclude", "reason": "fixture"},
        ],
    }
    genres = [{"genre_qid": qid, "present": True} for qid in
              ("Q1", "Q2", "Q3", "Q4", "Q5", "Q10")]
    parents = [{"genre_qid": child, "parent_qid": parent} for child, parent in
               (("Q2", "Q1"), ("Q3", "Q2"), ("Q4", "Q2"), ("Q5", "Q2"), ("Q10", "Q2"))]
    return genres, parents, config


def membership(artist, genre, source):
    return {"artist_mbid": artist, "genre_qid": genre, "source": source}


class RollupTests(unittest.TestCase):
    def test_resolver_chain_and_artist_precedence(self):
        genres, parents, config = fixture()
        rows = [membership("a", "Q3", "wikidata"), membership("a", "Q5", "manual"),
                membership("b", "Q4", "file_tag"), membership("b", "Q3", "wikidata"),
                membership("c", "Q10", "manual"), membership("c", "Q3", "wikidata")]
        result = resolve(genres, parents, rows, config)
        self.assertEqual(result["artist_roots"], [
            {"artist_mbid": "a", "root_id": "broad"},
            {"artist_mbid": "b", "root_id": "narrow"},
            {"artist_mbid": "b", "root_id": "broad"},
        ])
        self.assertIn({"genre_qid": "Q3", "root_id": "narrow"}, result["genre_rollup"])
        self.assertNotIn({"genre_qid": "Q3", "root_id": "broad"}, result["genre_rollup"])
        self.assertIn({"genre_qid": "Q4", "flag": "broad_multi"}, result["diagnostics"])
        self.assertIn({"genre_qid": "Q9", "flag": "override_target_missing"}, result["diagnostics"])
        self.assertIn({"genre_qid": "Q10", "reason": "excluded"}, result["diagnostics"])
        self.assertEqual(result, resolve(genres, list(reversed(parents)), list(reversed(rows)), config))

    def test_missing_target_is_inert_and_membership_is_reported(self):
        genres, parents, config = fixture()
        result = resolve(genres, parents, [membership("a", "Q9", "wikidata")], config)
        self.assertEqual(result["artist_roots"], [])
        self.assertIn({"genre_qid": "Q9", "reason": "unknown_genre"}, result["diagnostics"])
        self.assertIn({"genre_qid": "Q9", "flag": "override_target_missing"}, result["diagnostics"])

    def test_limits_and_barriers_propagate(self):
        genres, parents, config = fixture()
        config["limits"]["max_depth"] = 1
        result = resolve(genres, parents, [membership("a", "Q3", "wikidata")], config)
        self.assertIn({"genre_qid": "Q3", "flag": "traversal_capped"}, result["diagnostics"])
        config["traversal_barriers"] = ["Q3"]
        result = resolve(genres, parents, [membership("a", "Q3", "wikidata")], config)
        self.assertEqual(result["artist_roots"], [])
        self.assertIn({"genre_qid": "Q3", "reason": "no_root_reached"}, result["diagnostics"])

    def test_remove_diagnostic_requires_an_actually_removed_root(self):
        genres, parents, config = fixture()
        config["overrides"].extend([
            {"genre_qid": "Q3", "action": "remove", "roots": ["narrow"], "reason": "fixture"},
            {"genre_qid": "Q1", "action": "remove", "roots": ["narrow"], "reason": "fixture"},
        ])
        result = resolve(genres, parents, [membership("a", "Q3", "wikidata")], config)
        self.assertEqual(result["artist_roots"], [])
        self.assertIn({"genre_qid": "Q3", "reason": "removed"}, result["diagnostics"])
        self.assertNotIn({"genre_qid": "Q3", "reason": "no_root_reached"}, result["diagnostics"])
        config["traversal_barriers"] = ["Q3"]
        result = resolve(genres, parents, [], config)
        self.assertIn({"genre_qid": "Q3", "reason": "no_root_reached"}, result["diagnostics"])
        self.assertNotIn({"genre_qid": "Q3", "reason": "removed"}, result["diagnostics"])

    def test_invalid_rows_and_config_rejected(self):
        genres, parents, config = fixture()
        with self.assertRaises(ValueError):
            resolve(genres, parents, [membership("a", "Q3", [])], config)
        with self.assertRaises(ValueError):
            resolve(genres, [{"genre_qid": "Q0", "parent_qid": "Q1"}], [], config)
        config["limits"]["max_nodes"] = 0
        with self.assertRaises(ValueError):
            resolve(genres, parents, [], config)


class SyntheticIntegrationTests(unittest.TestCase):
    artist = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"

    def setUp(self):
        self.cohort = json.dumps({"schema": "nntune.genre-artists.v1",
                                  "dataset_kind": "synthetic",
                                  "artist_mbids": [self.artist.upper()],
                                  "playable_artist_mbids": [self.artist]}).encode()
        self.genres, self.parents, self.config = fixture()
        self.genres.extend([{"genre_qid": "Q101", "present": True},
                            {"genre_qid": "Q102", "present": True}])
        self.parents.extend([{"genre_qid": "Q101", "parent_qid": "Q2"},
                             {"genre_qid": "Q102", "parent_qid": "Q1"}])
        self.rows = [membership(self.artist.upper(), "Q101", "synthetic-fixture"),
                     membership(self.artist, "Q102", "synthetic-fixture")]

    def run_rows(self, rows):
        return synthetic_resolver.resolve_synthetic(
            self.cohort, self.genres, self.parents, rows, self.config)

    def test_resolves_normalized_rows_deterministically(self):
        result = self.run_rows(self.rows)
        self.assertEqual(result["artist_roots"], [
            {"artist_mbid": self.artist, "root_id": "narrow"},
            {"artist_mbid": self.artist, "root_id": "broad"}])
        self.assertEqual(result, self.run_rows(list(reversed(self.rows))))
        self.assertEqual(result, self.run_rows(self.rows))
        self.assertEqual(self.run_rows([])["artist_roots"], [])

    def test_rejects_unverified_duplicates_and_invalid_rows(self):
        invalid = [
            [self.rows[0], self.rows[0]],
            [membership(self.artist, "Q101", "wikidata")],
            [membership(self.artist, "Q101", "manual")],
            [membership(self.artist, "Q3", "synthetic-fixture")],
            [membership("22222222-2222-4222-8222-222222222222", "Q104", "synthetic-fixture")],
            [membership("bad", "Q101", "synthetic-fixture")],
            [membership(self.artist, "Q0", "synthetic-fixture")],
            [{**self.rows[0], "license": "CC0-1.0"}],
            [None], None,
        ]
        for rows in invalid:
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                self.run_rows(rows)
        with self.assertRaises(ValueError):
            synthetic_resolver.resolve_synthetic(b"{}", self.genres, self.parents,
                                                 self.rows, self.config)

    def test_membership_row_bound(self):
        with patch.object(synthetic_resolver, "MAX_ROWS", 1):
            self.run_rows(self.rows[:1])
            with self.assertRaises(ValueError):
                self.run_rows(self.rows)


if __name__ == "__main__":
    unittest.main()
