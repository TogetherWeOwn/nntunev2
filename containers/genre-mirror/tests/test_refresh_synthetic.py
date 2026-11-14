#!/usr/bin/env python3
"""Synthetic-only tests for the inert refresh SQL renderer."""

import json
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
import refresh_synthetic

EXPECTED = HERE.parents[1] / "genre-extract" / "tests" / "fixtures" / "expected"


def load(name):
    return [json.loads(line) for line in (EXPECTED / name).read_text("utf-8").splitlines()]


def config():
    return {"taxonomy_version": 1, "traversal_barriers": ["Q107", "Q106"],
            "limits": {"max_depth": 8, "max_nodes": 20, "broad_multi_threshold": 2},
            "roots": [{"root_id": "zeta", "display_name": "Zeta", "sort_order": 2, "anchors": ["Q102", "Q101"]},
                      {"root_id": "alpha", "display_name": "Alpha", "sort_order": 1, "anchors": ["Q100"]}],
            "overrides": [{"genre_qid": "Q102", "action": "add", "roots": ["zeta", "alpha"], "reason": "fixture"},
                          {"genre_qid": "Q101", "action": "add", "roots": ["alpha"], "reason": "fixture"}]}


DEFAULT = object()


class RefreshSyntheticTests(unittest.TestCase):
    def setUp(self):
        self.rows = [{**row, "source": "synthetic-fixture"} for row in load("artist_genres.jsonl")]
        artists = sorted({row["artist_mbid"] for row in self.rows})
        self.cohort = json.dumps({"schema": "nntune.genre-artists.v1", "dataset_kind": "synthetic",
                                  "artist_mbids": artists, "playable_artist_mbids": []}).encode()

    def render(self, rows=None, cfg=DEFAULT):
        return refresh_synthetic.synthetic_refresh_sql(
            self.cohort, self.rows if rows is None else rows, config() if cfg is DEFAULT else cfg)

    def test_constants_equal_extractor_fixture(self):
        self.assertEqual([{"genre_qid": q, "label": l, "present": p}
                          for q, l, p in refresh_synthetic.FIXTURE_GENRES], load("genres.jsonl"))
        self.assertEqual([{"genre_qid": c, "parent_qid": p}
                          for c, p in refresh_synthetic.FIXTURE_PARENTS], load("genre_parents.jsonl"))
        self.assertEqual({(r["artist_mbid"], r["genre_qid"]) for r in self.rows},
                         set(refresh_synthetic.FIXTURE_PAIRS))

    def test_deterministic_across_permutations(self):
        shuffled = config()
        shuffled["traversal_barriers"].reverse()
        shuffled["roots"].reverse()
        shuffled["overrides"].reverse()
        for root in shuffled["roots"]:
            root["anchors"].reverse()
        for row in shuffled["overrides"]:
            row["roots"].reverse()
        self.assertEqual(self.render(), self.render(list(reversed(self.rows)), shuffled))
        text = self.render().decode()
        self.assertTrue(text.startswith("BEGIN;\n") and text.endswith("COMMIT;\n"))
        self.assertIn("standard_conforming_strings = on", text)
        self.assertNotIn("TRUNCATE", text)

    def test_subset_refresh_removes_all_owned_fixture_rows(self):
        text = self.render(self.rows[:1]).decode()
        delete = text.split("DELETE FROM public.artist_genre")[1].split(";\n")[0]
        for row in self.rows:
            self.assertIn(row["artist_mbid"], delete)
            self.assertIn(row["genre_qid"], delete)
        insert = text.split("INSERT INTO public.artist_genre")[1].split(";\n")[0]
        self.assertNotIn(self.rows[1]["genre_qid"], insert.split("jsonb_to_recordset")[1])
        self.assertEqual(text.count("DELETE FROM"), 3)

    def test_overrides_replace_owned_fixture_scope(self):
        for overrides in ([{"genre_qid": "Q101", "action": "replace", "roots": ["alpha"],
                            "reason": "fixture"}], []):
            cfg = config()
            cfg["overrides"] = overrides
            text = self.render(cfg=cfg).decode()
            delete = text.split("DELETE FROM public.genre_override")[1].split(";\n")[0]
            self.assertIn('"genre_qid":"Q101"', delete)
            self.assertLess(text.index("DELETE FROM public.genre_override"),
                            text.index("INSERT INTO public.genre_override"))
            insert = text.split("INSERT INTO public.genre_override")[1].split(";\n")[0]
            self.assertNotIn('"action":"exclude"', insert)

    def test_rejects_malformed_and_non_fixture_input(self):
        bad = [[{**self.rows[0], "genre_qid": "Q999"}], [{**self.rows[0], "source": "wikidata"}],
               [self.rows[0], self.rows[0]], ["x"], {}, self.rows + self.rows,
               [{"artist_mbid": "bad", "genre_qid": "Q101", "source": "synthetic-fixture"}]]
        for rows in bad:
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                self.render(rows)
        outside = config()
        outside["overrides"].append({"genre_qid": "Q999", "action": "exclude", "reason": "fixture"})
        for cfg in ({}, [], None, outside):
            with self.subTest(cfg=cfg), self.assertRaises(ValueError):
                self.render(cfg=cfg)
        with patch.object(refresh_synthetic, "MAX_SQL_BYTES", 10), self.assertRaises(ValueError):
            self.render()


if __name__ == "__main__":
    unittest.main()
