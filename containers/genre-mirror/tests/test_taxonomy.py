"""Tests for containers/genre-mirror/taxonomy.py (stdlib only, synthetic QIDs)."""

import importlib.util
import os
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "genre_mirror_taxonomy", os.path.join(os.path.dirname(HERE), "taxonomy.py")
)
taxonomy = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(taxonomy)


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


BASE_GENRES = gen(1, 2, 3, 4, 10, 11, 12, 13, 14, 20)


class ConfigValidationTests(unittest.TestCase):
    def bad(self, mutate):
        cfg = make_config()
        mutate(cfg)
        with self.assertRaises(ValueError):
            taxonomy.validate_config(cfg, BASE_GENRES)

    def test_valid(self):
        cfg = make_config()
        self.assertEqual(taxonomy.validate_config(cfg, BASE_GENRES), cfg)

    def test_top_level_shape(self):
        for bad in (None, [], "x", {}):
            with self.assertRaises(ValueError):
                taxonomy.validate_config(bad, BASE_GENRES)
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

    def test_retired_root_cannot_be_override_target(self):
        cfg = make_config(roots=[root("retired", 1, retired=True)])
        taxonomy.validate_config(cfg, [])
        cfg["overrides"] = [self.ov(10, "add", ["retired"])]
        with self.assertRaises(ValueError):
            taxonomy.validate_config(cfg, [])

    def test_active_root_needs_present_anchor(self):
        genres = gen(2, 3, 4) + gen(absent=(1,))
        with self.assertRaises(ValueError):
            taxonomy.validate_config(make_config(), genres)
        with self.assertRaises(ValueError):
            taxonomy.validate_config(make_config(), gen(2, 3))
        # one present anchor among several suffices
        cfg = make_config(roots=[root("rock", 1, 1, 2), root("jazz", 3, 3), root("folk", 4, 4)])
        taxonomy.validate_config(cfg, gen(2, 3, 4))

    def test_genres_shape(self):
        for g in (None, [None], [{"genre_qid": "x", "present": True}],
                  [{"genre_qid": q(1), "present": "yes"}]):
            with self.assertRaises(ValueError):
                taxonomy.validate_config(make_config(), g)

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
            taxonomy.validate_config(make_config(overrides=[o]), BASE_GENRES)

    def test_override_duplicates_and_combinations(self):
        self.bad(lambda c: c.update(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "remove", ["rock", "jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "replace", ["rock"]), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "replace", ["rock"]), self.ov(10, "remove", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "exclude"), self.ov(10, "add", ["jazz"])]))
        self.bad(lambda c: c.update(overrides=[self.ov(10, "exclude"), self.ov(10, "replace", ["jazz"])]))
        ok = make_config(overrides=[self.ov(10, "add", ["rock"]), self.ov(10, "remove", ["jazz"])])
        taxonomy.validate_config(ok, BASE_GENRES)

    def test_error_messages_are_fixed(self):
        cfg = make_config()
        cfg["roots"][0]["display_name"] = "SECRET-VALUE"
        cfg["roots"][1]["root_id"] = "BAD ID SECRET"
        with self.assertRaises(ValueError) as cm:
            taxonomy.validate_config(cfg, BASE_GENRES)
        self.assertNotIn("SECRET", str(cm.exception))


if __name__ == "__main__":
    unittest.main()
