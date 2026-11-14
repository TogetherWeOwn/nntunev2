"""Synthetic integration tests for the offline resolver chain."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from rollup import resolve


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

    def test_invalid_rows_and_config_rejected(self):
        genres, parents, config = fixture()
        with self.assertRaises(ValueError):
            resolve(genres, parents, [membership("a", "Q3", [])], config)
        with self.assertRaises(ValueError):
            resolve(genres, [{"genre_qid": "Q0", "parent_qid": "Q1"}], [], config)
        config["limits"]["max_nodes"] = 0
        with self.assertRaises(ValueError):
            resolve(genres, parents, [], config)


if __name__ == "__main__":
    unittest.main()
