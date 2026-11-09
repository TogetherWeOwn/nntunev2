"""Tests for containers/genre-mirror/rollup.py (stdlib only, synthetic QIDs)."""

import copy
import importlib.util
import json
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "genre_mirror_rollup", os.path.join(os.path.dirname(HERE), "rollup.py")
)
rollup = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(rollup)


def q(n):
    return "Q%d" % (900000 + n)


def root(rid, sort, *anchors, **extra):
    r = {
        "root_id": rid,
        "display_name": rid.title(),
        "sort_order": sort,
        "anchors": [q(a) for a in anchors],
    }
    r.update(extra)
    return r


def make_config(**kw):
    cfg = {
        "taxonomy_version": 1,
        "traversal_barriers": [],
        "limits": {"max_depth": 32, "max_nodes": 10000, "broad_multi_threshold": 3},
        "roots": [
            root("rock", 1, 1),
            root("metal", 2, 2),
            root("jazz", 3, 3),
            root("folk", 4, 4),
        ],
        "overrides": [],
    }
    cfg.update(kw)
    return cfg


def gen(*nums, absent=()):
    out = [{"genre_qid": q(n), "label": "g%d" % n, "present": True} for n in nums]
    out += [{"genre_qid": q(n), "label": "g%d" % n, "present": False} for n in absent]
    return out


def edges(*pairs):
    return [{"genre_qid": q(a), "parent_qid": q(b)} for a, b in pairs]


def mem(artist, n, source="wikidata"):
    return {"artist_mbid": artist, "genre_qid": q(n), "source": source}


BASE_GENRES = gen(1, 2, 3, 4, 10, 11, 12, 13, 14, 20)
BASE_EDGES = edges((10, 1), (11, 10), (12, 1), (12, 3), (2, 1), (13, 2))


def roots_of(result, n):
    return [r["root_id"] for r in result["genre_rollup"] if r["genre_qid"] == q(n)]


def diag_of(result, n):
    return [d for d in result["diagnostics"] if d["genre_qid"] == q(n)]


def run(genres=None, parents=None, memberships=None, config=None):
    return rollup.resolve(
        BASE_GENRES if genres is None else genres,
        BASE_EDGES if parents is None else parents,
        [] if memberships is None else memberships,
        make_config() if config is None else config,
    )


class ResolveTests(unittest.TestCase):
    def test_anchor_is_its_own_root(self):
        self.assertEqual(roots_of(run(), 1), ["rock"])
        self.assertEqual(roots_of(run(), 3), ["jazz"])

    def test_descent_reaches_root(self):
        self.assertEqual(roots_of(run(), 10), ["rock"])
        self.assertEqual(roots_of(run(), 11), ["rock"])

    def test_siblings_multi_root_in_sort_order(self):
        self.assertEqual(roots_of(run(), 12), ["rock", "jazz"])

    def test_most_specific_subsumption(self):
        res = run()
        self.assertEqual(roots_of(res, 2), ["metal"])
        self.assertEqual(roots_of(res, 13), ["metal"])

    def test_multi_anchor_root(self):
        cfg = make_config(roots=[root("rock", 1, 1, 10), root("jazz", 3, 3)])
        res = run(config=cfg)
        self.assertEqual(roots_of(res, 11), ["rock"])

    def test_mutual_root_cycle_keeps_both(self):
        genres = gen(1, 5, 6, 14)
        parents = edges((5, 6), (6, 5), (14, 5))
        cfg = make_config(roots=[root("rock", 1, 1), root("ma", 5, 5), root("mb", 6, 6)])
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 14), ["ma", "mb"])
        self.assertIn({"genre_qid": q(14), "flag": "cycle_detected"}, res["diagnostics"])

    def test_multi_anchor_projected_nesting_cycle_keeps_roots(self):
        cfg = make_config(roots=[root("a", 1, 1, 2), root("b", 2, 3, 4), root("c", 3, 5, 6)])
        genres = gen(1, 2, 3, 4, 5, 6, 7)
        parents = edges((1, 3), (4, 5), (6, 2), (7, 1), (7, 4), (7, 6))
        res = run(genres, parents, [mem("artist", 7)], cfg)
        self.assertEqual(roots_of(res, 7), ["a", "b", "c"])
        self.assertEqual(len(res["artist_roots"]), 3)
        self.assertEqual(diag_of(res, 7), [], "acyclic ancestry must not be flagged as a genre cycle")

    def test_real_cycle_flagged_diamond_not(self):
        genres = gen(1, 2, 3, 4, 30, 31, 32, 33, 34)
        # diamond: 30 -> 31,32 -> 33 -> 1
        parents = edges((30, 31), (30, 32), (31, 33), (32, 33), (33, 1))
        res = run(genres, parents)
        self.assertEqual(diag_of(res, 30), [])
        self.assertEqual(roots_of(res, 30), ["rock"])
        cyc = edges((30, 31), (31, 30), (31, 1))
        res = run(genres, cyc)
        self.assertEqual(diag_of(res, 30), [{"genre_qid": q(30), "flag": "cycle_detected"}])
        self.assertEqual(roots_of(res, 30), ["rock"])
        self_loop = edges((34, 34), (34, 1))
        res = run(genres, self_loop)
        self.assertIn({"genre_qid": q(34), "flag": "cycle_detected"}, res["diagnostics"])

    def test_barrier_contributes_nothing_and_not_expanded(self):
        genres = gen(1, 2, 3, 4, 40, 41, 42)
        parents = edges((40, 41), (41, 1), (42, 1))
        cfg = make_config(traversal_barriers=[q(41)])
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 40), [])
        self.assertEqual(diag_of(res, 40), [{"genre_qid": q(40), "reason": "no_root_reached"}])
        self.assertEqual(roots_of(res, 42), ["rock"])
        self.assertEqual(roots_of(res, 41), [])

    def test_barrier_not_a_cap(self):
        genres = gen(1, 2, 3, 4, 40, 41)
        parents = edges((40, 41), (41, 1))
        res = run(genres, parents, config=make_config(traversal_barriers=[q(41)]))
        self.assertFalse(any(d.get("flag") == "traversal_capped" for d in res["diagnostics"]))

    def test_missing_parent_leaf_contributes_anchor(self):
        genres = gen(1, 2, 3, 60) + gen(absent=(4,))
        cfg = make_config(roots=[root("rock", 1, 1, 4), root("jazz", 3, 3)])
        res = run(genres, edges((60, 4)), config=cfg)
        self.assertEqual(roots_of(res, 60), ["rock"])
        self.assertEqual(roots_of(res, 4), ["rock"])

    def test_depth_cap(self):
        genres = gen(1, 2, 3, 4, 70, 71, 72, 73)
        parents = edges((73, 72), (72, 71), (71, 70), (70, 1))
        cfg = make_config()
        cfg["limits"]["max_depth"] = 2
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 73), [])
        self.assertIn({"genre_qid": q(73), "flag": "traversal_capped"}, res["diagnostics"])
        # exactly 2 edges reaches root, and is not capped (natural end at root)
        self.assertEqual(roots_of(res, 71), ["rock"])
        self.assertEqual(diag_of(res, 71), [])

    def test_depth_cap_keeps_found_roots(self):
        genres = gen(1, 2, 3, 4, 70, 71, 72)
        parents = edges((72, 3), (72, 71), (71, 70), (70, 1))
        cfg = make_config()
        cfg["limits"]["max_depth"] = 2
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 72), ["jazz"])
        self.assertIn({"genre_qid": q(72), "flag": "traversal_capped"}, res["diagnostics"])

    def test_node_cap(self):
        genres = gen(1, 2, 3, 4, 80)
        parents = edges((80, 1), (80, 3))
        cfg = make_config()
        cfg["limits"]["max_nodes"] = 2
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 80), ["rock"])
        self.assertIn({"genre_qid": q(80), "flag": "traversal_capped"}, res["diagnostics"])
        cfg["limits"]["max_nodes"] = 3
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 80), ["rock", "jazz"])
        self.assertEqual(diag_of(res, 80), [])

    def test_empty_reasons(self):
        genres = gen(1, 2, 3, 4, 90, 91) + gen(absent=(92,))
        parents = edges((91, 93))
        res = run(genres, parents, [mem("a", 94)])
        self.assertEqual(diag_of(res, 90), [{"genre_qid": q(90), "reason": "no_parents"}])
        self.assertEqual(diag_of(res, 91), [{"genre_qid": q(91), "reason": "no_root_reached"}])
        self.assertEqual(diag_of(res, 92), [{"genre_qid": q(92), "reason": "unknown_genre"}])
        # membership-referenced absent node is an unknown genre
        self.assertEqual(diag_of(res, 94), [{"genre_qid": q(94), "reason": "unknown_genre"}])

    def test_broad_multi(self):
        genres = gen(1, 2, 3, 4, 95)
        cfg = make_config()
        cfg["roots"] = [root("a", 1, 1), root("b", 2, 2), root("c", 3, 3), root("d", 4, 4)]
        parents = edges((95, 1), (95, 2), (95, 3), (95, 4))
        res = run(genres, parents, config=cfg)
        self.assertEqual(roots_of(res, 95), ["a", "b", "c", "d"])
        self.assertEqual(diag_of(res, 95), [{"genre_qid": q(95), "flag": "broad_multi"}])
        cfg["limits"]["broad_multi_threshold"] = 4
        self.assertEqual(diag_of(run(genres, parents, config=cfg), 95), [])


class OverrideTests(unittest.TestCase):
    def ov(self, qn, action, roots=None, reason="because"):
        o = {"genre_qid": q(qn), "action": action, "reason": reason}
        if roots is not None:
            o["roots"] = roots
        return o

    def test_exclude(self):
        cfg = make_config(overrides=[self.ov(10, "exclude")])
        res = run(config=cfg)
        self.assertEqual(roots_of(res, 10), [])
        self.assertEqual(diag_of(res, 10), [{"genre_qid": q(10), "reason": "excluded"}])
        self.assertEqual(roots_of(res, 11), ["rock"])

    def test_replace_exact(self):
        cfg = make_config(overrides=[self.ov(12, "replace", ["folk"])])
        self.assertEqual(roots_of(run(config=cfg), 12), ["folk"])

    def test_add_does_not_resubsume(self):
        cfg = make_config(overrides=[self.ov(13, "add", ["rock"])])
        self.assertEqual(roots_of(run(config=cfg), 13), ["rock", "metal"])

    def test_remove(self):
        cfg = make_config(overrides=[self.ov(12, "remove", ["rock"])])
        self.assertEqual(roots_of(run(config=cfg), 12), ["jazz"])

    def test_add_and_remove_disjoint(self):
        cfg = make_config(
            overrides=[self.ov(12, "add", ["folk"]), self.ov(12, "remove", ["rock"])]
        )
        self.assertEqual(roots_of(run(config=cfg), 12), ["jazz", "folk"])

    def test_add_gives_root_to_unrooted(self):
        cfg = make_config(overrides=[self.ov(20, "add", ["folk"])])
        res = run(config=cfg)
        self.assertEqual(roots_of(res, 20), ["folk"])
        self.assertEqual(diag_of(res, 20), [])

    def test_missing_override_target_warning(self):
        genres = gen(1, 2, 3, 4) + gen(absent=(21,))
        cfg = make_config(overrides=[self.ov(21, "add", ["folk"]), self.ov(22, "exclude")])
        res = run(genres, [], config=cfg)
        self.assertIn({"genre_qid": q(21), "flag": "override_target_missing"}, res["diagnostics"])
        self.assertIn({"genre_qid": q(22), "flag": "override_target_missing"}, res["diagnostics"])
        self.assertNotIn(q(22), [r["genre_qid"] for r in res["genre_rollup"]])
        self.assertEqual(roots_of(res, 21), [], "missing override target must be inert")
        cfg["overrides"][0] = self.ov(21, "replace", ["folk"])
        res = run(genres, [], config=cfg)
        self.assertEqual(roots_of(res, 21), [], "missing replace target must also be inert")

    def test_retired_root_not_emitted_and_reserved(self):
        cfg = make_config(roots=make_config()["roots"] + [root("old", 9, 10, retired=True)])
        res = run(config=cfg)
        self.assertEqual(roots_of(res, 10), ["rock"])
        self.assertNotIn("old", [r["root_id"] for r in res["genre_rollup"]])
        cfg["overrides"] = [self.ov(20, "add", ["old"])]
        with self.assertRaises(ValueError):
            run(config=cfg)
        cfg["roots"].append(root("old", 10, 11))
        cfg["overrides"] = []
        with self.assertRaises(ValueError):
            run(config=cfg)

    def test_retired_anchor_absent_ok(self):
        cfg = make_config(roots=make_config()["roots"] + [root("old", 9, 777, retired=True)])
        rollup.validate_config(cfg, BASE_GENRES)


class ArtistTests(unittest.TestCase):
    def test_union_of_wikidata_and_file_tag(self):
        res = run(memberships=[mem("a1", 10), mem("a1", 3, "file_tag"), mem("a1", 3)])
        self.assertEqual(
            res["artist_roots"],
            [
                {"artist_mbid": "a1", "root_id": "rock"},
                {"artist_mbid": "a1", "root_id": "jazz"},
            ],
        )

    def test_manual_wins_even_unresolved(self):
        res = run(memberships=[mem("a1", 10), mem("a1", 20, "manual"), mem("a2", 10)])
        self.assertEqual(res["artist_roots"], [{"artist_mbid": "a2", "root_id": "rock"}])

    def test_manual_resolves(self):
        res = run(memberships=[mem("a1", 10), mem("a1", 3, "manual")])
        self.assertEqual(res["artist_roots"], [{"artist_mbid": "a1", "root_id": "jazz"}])

    def test_artist_sorted(self):
        res = run(memberships=[mem("b", 3), mem("a", 3)])
        self.assertEqual([r["artist_mbid"] for r in res["artist_roots"]], ["a", "b"])

    def test_bad_source_rejected(self):
        with self.assertRaises(ValueError):
            run(memberships=[mem("a", 1, "scraped")])


class DeterminismTests(unittest.TestCase):
    def test_byte_deterministic_across_input_order(self):
        mems = [mem("b", 12), mem("a", 13), mem("a", 94), mem("c", 3, "manual")]
        one = run(memberships=mems)
        two = rollup.resolve(
            list(reversed(BASE_GENRES)),
            list(reversed(BASE_EDGES)),
            list(reversed(mems)),
            make_config(),
        )
        enc = lambda r: json.dumps(r, sort_keys=False, separators=(",", ":")).encode()
        self.assertEqual(enc(one), enc(two))
        self.assertEqual(set(one), {"genre_rollup", "artist_roots", "diagnostics"})

    def test_numeric_qid_order(self):
        genres = gen(1, 2, 3, 4, 9, 10, 100)
        res = run(genres, edges((9, 1), (10, 1), (100, 1)))
        order = [r["genre_qid"] for r in res["genre_rollup"]]
        self.assertEqual(order[-3:], [q(9), q(10), q(100)])
        self.assertEqual(order, sorted(order, key=lambda s: int(s[1:])))

    def test_input_not_mutated(self):
        cfg = make_config()
        before = copy.deepcopy(cfg)
        run(config=cfg)
        self.assertEqual(cfg, before)


class ConfigValidationTests(unittest.TestCase):
    def bad(self, mutate):
        cfg = make_config()
        mutate(cfg)
        with self.assertRaises(ValueError):
            rollup.validate_config(cfg, BASE_GENRES)

    def test_valid(self):
        cfg = make_config()
        self.assertEqual(rollup.validate_config(cfg, BASE_GENRES), cfg)

    def test_top_level_shape(self):
        for bad in (None, [], "x", {}):
            with self.assertRaises(ValueError):
                rollup.validate_config(bad, BASE_GENRES)
        self.bad(lambda c: c.update(extra=1))
        self.bad(lambda c: c.pop("overrides"))
        self.bad(lambda c: c.update(taxonomy_version=2))
        self.bad(lambda c: c.update(taxonomy_version=True))
        self.bad(lambda c: c.update(taxonomy_version="1"))

    def test_barriers(self):
        self.bad(lambda c: c.update(traversal_barriers="Q1"))
        self.bad(lambda c: c.update(traversal_barriers=["q900001"]))
        self.bad(lambda c: c.update(traversal_barriers=[q(50), q(50)]))
        self.bad(lambda c: c.update(traversal_barriers=[q(1)]))  # anchor
        self.bad(lambda c: c.update(traversal_barriers=["Q0"]))
        self.bad(lambda c: c.update(traversal_barriers=["Q9\n"]))

    def test_limits(self):
        for key, vals in (
            ("max_depth", [0, -1, 65, 1.5, True, "32", None]),
            ("max_nodes", [0, 100001, 2.0, False]),
            ("broad_multi_threshold", [0, 65, "3"]),
        ):
            for v in vals:
                self.bad(lambda c, key=key, v=v: c["limits"].__setitem__(key, v))
        self.bad(lambda c: c["limits"].pop("max_depth"))
        self.bad(lambda c: c["limits"].update(extra=1))
        self.bad(lambda c: c.update(limits=[]))

    def test_roots_shape(self):
        self.bad(lambda c: c.update(roots=[]))
        self.bad(lambda c: c.update(roots={}))
        self.bad(lambda c: c["roots"].append("x"))
        self.bad(lambda c: c["roots"][0].update(extra=1))
        self.bad(lambda c: c["roots"][0].pop("display_name"))
        self.bad(lambda c: c.update(roots=[root("r%d" % i, i + 1, 100 + i) for i in range(65)]))

    def test_root_id(self):
        for rid in ("", "Rock", "a-b", "a" * 33, "a b", 5, None, "rock\n", "é"):
            self.bad(lambda c, rid=rid: c["roots"][0].update(root_id=rid))
        self.bad(lambda c: c["roots"][1].update(root_id="rock"))

    def test_display_name(self):
        for v in ("", "  ", 5, "x" * 65, "a\nb"):
            self.bad(lambda c, v=v: c["roots"][0].update(display_name=v))

    def test_sort_order(self):
        for v in (0, -1, "1", 1.0, True, 1000001, None):
            self.bad(lambda c, v=v: c["roots"][0].update(sort_order=v))
        self.bad(lambda c: c["roots"][1].update(sort_order=1))

    def test_anchors(self):
        self.bad(lambda c: c["roots"][0].update(anchors=[]))
        self.bad(lambda c: c["roots"][0].update(anchors="Q900001"))
        self.bad(lambda c: c["roots"][0].update(anchors=["x"]))
        self.bad(lambda c: c["roots"][0].update(anchors=[q(1), q(1)]))
        self.bad(lambda c: c["roots"][1].update(anchors=[q(1)]))  # shared anchor
        self.bad(lambda c: c["roots"][0].update(anchors=[q(i) for i in range(1, 66)]))

    def test_retired_flag(self):
        self.bad(lambda c: c["roots"][0].update(retired="yes"))
        self.bad(lambda c: c["roots"][0].update(retired=1))

    def test_active_root_needs_present_anchor(self):
        genres = gen(2, 3, 4) + gen(absent=(1,))
        with self.assertRaises(ValueError):
            rollup.validate_config(make_config(), genres)
        with self.assertRaises(ValueError):
            rollup.validate_config(make_config(), gen(2, 3))
        # one present anchor among several suffices
        cfg = make_config(roots=[root("rock", 1, 1, 2), root("jazz", 3, 3), root("folk", 4, 4)])
        rollup.validate_config(cfg, gen(2, 3, 4))

    def test_genres_shape(self):
        for g in (None, [None], [{"genre_qid": "x", "present": True}],
                  [{"genre_qid": q(1), "present": "yes"}]):
            with self.assertRaises(ValueError):
                rollup.validate_config(make_config(), g)

    def ov(self, qn, action, roots=None, reason="why"):
        o = {"genre_qid": q(qn), "action": action, "reason": reason}
        if roots is not None:
            o["roots"] = roots
        return o

    def test_override_shape(self):
        for o in (
            "x",
            self.ov(10, "bogus", ["rock"]),
            self.ov(10, "add", ["nope"]),
            self.ov(10, "add", []),
            self.ov(10, "add"),
            self.ov(10, "add", ["rock", "rock"]),
            self.ov(10, "add", "rock"),
            self.ov(10, "add", ["rock"], reason=""),
            self.ov(10, "add", ["rock"], reason="  "),
            self.ov(10, "add", ["rock"], reason=5),
            self.ov(10, "add", ["rock"], reason="x" * 201),
            self.ov(10, "exclude", ["rock"]),
            {**self.ov(10, "add", ["rock"]), "extra": 1},
            {**self.ov(10, "add", ["rock"]), "genre_qid": "10"},
            {k: v for k, v in self.ov(10, "add", ["rock"]).items() if k != "reason"},
        ):
            self.bad(lambda c, o=o: c.update(overrides=[o]))
        self.bad(lambda c: c.update(overrides={}))

    def test_override_exclude_roots_optional_empty(self):
        for o in (self.ov(10, "exclude"), self.ov(10, "exclude", [])):
            rollup.validate_config(make_config(overrides=[o]), BASE_GENRES)

    def test_override_duplicates_and_combinations(self):
        self.bad(lambda c: c.update(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "remove", ["rock", "jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "replace", ["rock"]), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "replace", ["rock"]), self.ov(10, "remove", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "exclude"), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "exclude"), self.ov(10, "replace", ["jazz"])]))
        ok = make_config(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "remove", ["jazz"])])
        rollup.validate_config(ok, BASE_GENRES)

    def test_resolve_validates_config(self):
        cfg = make_config()
        cfg["limits"]["max_depth"] = 0
        with self.assertRaises(ValueError):
            run(config=cfg)

    def test_error_messages_are_fixed(self):
        cfg = make_config()
        cfg["roots"][0]["display_name"] = "SECRET-VALUE"
        cfg["roots"][1]["root_id"] = "BAD ID SECRET"
        with self.assertRaises(ValueError) as cm:
            rollup.validate_config(cfg, BASE_GENRES)
        self.assertNotIn("SECRET", str(cm.exception))


if __name__ == "__main__":
    unittest.main()
