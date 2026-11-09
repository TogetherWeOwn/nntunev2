"""Black-box CLI tests for containers/genre-mirror/materialize.py (stdlib only).

The existing extractor is run over the committed synthetic fixture; all roots,
names and the cohort below are invented. Nothing here touches a database.
"""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = Path(__file__).resolve().parent
MIRROR = HERE.parent
MATERIALIZE = MIRROR / "materialize.py"
EXTRACT = MIRROR.parent / "genre-extract" / "extract.py"
FIXTURE = MIRROR.parent / "genre-extract" / "tests" / "fixtures" / "synthetic.json"
TIMEOUT = 120
REFUSAL = "genre-mirror: refused invalid, incomplete or over-budget input/output\n"

A = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
B = "22222222-2222-4222-8222-222222222222"
C = "66666666-6666-4666-8666-666666666666"
D = "55555555-5555-4555-8555-555555555555"
REASON = "it's a \\ test ünï ☃"


def config():
    return {
        "taxonomy_version": 1,
        "traversal_barriers": [],
        "limits": {"max_depth": 32, "max_nodes": 10000, "broad_multi_threshold": 3},
        "roots": [
            {"root_id": "test_root", "display_name": "Test Root", "sort_order": 1, "anchors": ["Q100"]},
            {"root_id": "test_branch", "display_name": "Test Branch", "sort_order": 2, "anchors": ["Q102"]},
            {"root_id": "test_loop", "display_name": "Test Loop", "sort_order": 3, "anchors": ["Q105"]},
            {"root_id": "test_old", "display_name": "Test Old", "sort_order": 4,
             "anchors": ["Q900009"], "retired": True},
        ],
        "overrides": [{"genre_qid": "Q900123", "action": "exclude", "reason": REASON}],
    }


def cohort(artists=(A, B, C, D), playable=(A, C)):
    return {"schema": "nntune.genre-artists.v1", "dataset_kind": "synthetic",
            "artist_mbids": list(artists), "playable_artist_mbids": list(playable)}


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def run_py(script, *args):
    return subprocess.run([sys.executable, "-I", str(script), *map(str, args)], stdin=subprocess.DEVNULL,
                          capture_output=True, timeout=TIMEOUT, cwd=tempfile.gettempdir(),
                          env={"PATH": os.environ.get("PATH", "")})


def write_json(path, value):
    Path(path).write_bytes(json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n")


def tree(path):
    return {p.name: p.read_bytes() for p in sorted(Path(path).iterdir())}


def mutate(directory, name, raw):
    """Replace one extract file and keep its manifest receipt self-consistent."""
    directory = Path(directory)
    (directory / name).write_bytes(raw)
    manifest = json.loads((directory / "manifest.json").read_text("utf-8"))
    manifest["outputs"][name] = {"sha256": sha(raw), "bytes": len(raw), "rows": raw.count(b"\n")}
    write_json(directory / "manifest.json", manifest)


def jsonl(rows):
    return b"".join(json.dumps(r, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8") + b"\n"
                    for r in rows)


def read_rows(directory, name):
    return [json.loads(x) for x in (Path(directory) / name).read_bytes().splitlines()]


class MaterializeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory(prefix="genre-mirror-test-")
        cls.root = Path(cls._tmp.name)
        cls.extract = cls.root / "extract"
        done = run_py(EXTRACT, "--input", FIXTURE, "--output", cls.extract, "--dump-version", "synthetic-v1")
        assert done.returncode == 0, "fixture extraction failed"
        cls.config_path = cls.root / "config.json"
        cls.cohort_path = cls.root / "cohort.json"
        write_json(cls.config_path, config())
        write_json(cls.cohort_path, cohort())
        cls.counter = 0

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def fresh(self, name):
        MaterializeTests.counter += 1
        return self.root / ("%s-%d" % (name, MaterializeTests.counter))

    def materialize(self, extract=None, config_path=None, cohort_path=None, output=None):
        output = output or self.fresh("out")
        done = run_py(MATERIALIZE, "--extract", extract or self.extract, "--config", config_path or self.config_path,
                      "--cohort", cohort_path or self.cohort_path, "--output", output)
        return done, output

    def ok(self, **kw):
        done, output = self.materialize(**kw)
        self.assertEqual(done.returncode, 0, done.stderr)
        return output

    def refused(self, **kw):
        done, output = self.materialize(**kw)
        self.assertEqual(done.returncode, 2)
        self.assertEqual(done.stderr.decode(), REFUSAL)
        self.assertEqual(done.stdout, b"")
        return output

    def extract_copy(self):
        target = self.fresh("extract")
        shutil.copytree(self.extract, target)
        return target

    def test_outputs_checksums_and_provenance(self):
        out = self.ok()
        files = tree(out)
        self.assertEqual(set(files), {"coverage.json", "diagnostics.json", "genre_rollup.jsonl", "refresh.sql",
                                      "extract-manifest.json", "manifest.json"})
        manifest = json.loads(files["manifest.json"])
        self.assertEqual(manifest["schema"], "nntune.genre-mirror.v1")
        self.assertEqual(manifest["extract_manifest_sha256"], sha((self.extract / "manifest.json").read_bytes()))
        self.assertEqual(manifest["config_sha256"], sha(self.config_path.read_bytes()))
        self.assertEqual(manifest["cohort_sha256"], sha(self.cohort_path.read_bytes()))
        self.assertEqual(manifest["dataset_kind"], "synthetic")
        self.assertEqual(manifest["real_data_gate"], "not_evaluated")
        self.assertEqual(manifest["source"]["license"], "CC0-1.0")
        self.assertEqual(manifest["taxonomy_version"], 1)
        self.assertEqual(set(manifest["outputs"]), set(files) - {"manifest.json"})
        for name, receipt in manifest["outputs"].items():
            self.assertEqual(receipt, {"sha256": sha(files[name]), "bytes": len(files[name])}, name)

    def test_byte_deterministic_across_directories(self):
        first, second = self.ok(), self.ok()
        self.assertNotEqual(first, second)
        self.assertEqual(tree(first), tree(second))
        for raw in tree(first).values():
            self.assertNotIn(str(self.root).encode(), raw)

    def test_rollup_keeps_canonical_underscore_ids(self):
        rows = read_rows(self.ok(), "genre_rollup.jsonl")
        by_genre = {}
        for r in rows:
            by_genre.setdefault(r["genre_qid"], []).append(r["root_id"])
        self.assertEqual(by_genre["Q101"], ["test_root"])
        self.assertEqual(by_genre["Q102"], ["test_branch"])  # most specific wins
        self.assertEqual(by_genre["Q105"], ["test_loop"])
        self.assertEqual(by_genre["Q106"], ["test_loop"])
        self.assertNotIn("Q104", by_genre)
        self.assertFalse(any("-" in r["root_id"] or r["root_id"] == "test_old" for r in rows))

    def test_coverage_transport_hyphen_mapping(self):
        out = self.ok()
        coverage = json.loads((out / "coverage.json").read_text("utf-8"))
        self.assertEqual(coverage, {
            "schema_version": 1,
            "dataset_kind": "synthetic",
            "playable_artist_mbids": [C, A],
            "resolved_root_ids": ["test-branch", "test-loop", "test-root"],
            "memberships": [
                {"artist_mbid": C, "root_id": "test-loop"},
                {"artist_mbid": A, "root_id": "test-root"},
                {"artist_mbid": A, "root_id": "test-branch"},
            ],
        })
        manifest = json.loads((out / "manifest.json").read_text("utf-8"))
        self.assertEqual(manifest["coverage_root_id_map"],
                         {"test_branch": "test-branch", "test_loop": "test-loop", "test_root": "test-root"})
        self.assertEqual(manifest["unmatched_artist_mbids"], [])

    def test_unmatched_artists_filtered_from_coverage_only(self):
        path = self.root / "cohort-partial.json"
        write_json(path, cohort(artists=(A, B), playable=(A,)))
        out = self.ok(cohort_path=path)
        coverage = json.loads((out / "coverage.json").read_text("utf-8"))
        self.assertEqual({m["artist_mbid"] for m in coverage["memberships"]}, {A})
        self.assertEqual(coverage["playable_artist_mbids"], [A])
        manifest = json.loads((out / "manifest.json").read_text("utf-8"))
        self.assertEqual(manifest["unmatched_artist_mbids"], [C])

    def test_manual_and_file_tag_never_generated(self):
        out = self.ok()
        sql = (out / "refresh.sql").read_text("utf-8")
        self.assertIn("'wikidata'", sql)
        self.assertIn("DELETE FROM public.artist_genre WHERE source='wikidata';", sql)
        self.assertNotIn("manual", sql)
        self.assertNotIn("file_tag", sql)
        self.assertNotIn("manual", (out / "coverage.json").read_text("utf-8"))

    def test_sql_literals_escape_apostrophes_backslashes_unicode(self):
        extract = self.extract_copy()
        genres = read_rows(extract, "genres.jsonl")
        label = "O'Brien \\ back\\\\slash ünï ☃ ''; DROP TABLE x; --"
        for row in genres:
            if row["genre_qid"] == "Q100":
                row["label"] = label
        mutate(extract, "genres.jsonl", jsonl(genres))
        sql = (self.ok(extract=extract) / "refresh.sql").read_text("utf-8")
        self.assertLess(sql.index("standard_conforming_strings = on"), sql.index("::jsonb"))
        literals = re.findall(r"'((?:[^']|'')*)'::jsonb", sql)
        self.assertGreaterEqual(len(literals), 6)
        parsed = [json.loads(x.replace("''", "'")) for x in literals]
        labels = [e["label"] for v in parsed if isinstance(v, list) for e in v
                  if isinstance(e, dict) and e.get("genre_qid") == "Q100" and "label" in e]
        self.assertEqual(labels, [label])
        configs = [v for v in parsed if isinstance(v, dict) and "overrides" in v]
        self.assertEqual(configs[0]["overrides"][0]["reason"], REASON)
        # No unescaped single quote survives inside any literal body.
        for body in literals:
            self.assertNotRegex(body.replace("''", ""), "'")

    def test_tampered_or_malformed_extract_refused(self):
        genres = read_rows(self.extract, "genres.jsonl")
        parents = (self.extract / "genre_parents.jsonl").read_bytes()
        members = read_rows(self.extract, "artist_genres.jsonl")

        def flip(d):
            p = d / "genres.jsonl"
            raw = p.read_bytes()
            p.write_bytes(raw.replace(b"alpha", b"alphb"))

        def rows_plus_one(d):
            m = json.loads((d / "manifest.json").read_text("utf-8"))
            m["outputs"]["genre_parents.jsonl"]["rows"] += 1
            write_json(d / "manifest.json", m)

        def edit_manifest(key_path, value):
            def go(d):
                m = json.loads((d / "manifest.json").read_text("utf-8"))
                node = m
                for k in key_path[:-1]:
                    node = node[k]
                node[key_path[-1]] = value
                write_json(d / "manifest.json", m)
            return go

        cases = {
            "hash tamper": flip,
            "row count": rows_plus_one,
            "missing manifest": lambda d: (d / "manifest.json").unlink(),
            "missing data file": lambda d: (d / "genre_parents.jsonl").unlink(),
            "unknown version": edit_manifest(["extractor_version"], 2),
            "boolean version": edit_manifest(["extractor_version"], True),
            "invalid input identity": edit_manifest(["input", "sha256"], "bad"),
            "invalid limit count": edit_manifest(["limits", "max_depth"], True),
            "oversized receipt": edit_manifest(["outputs", "genres.jsonl", "bytes"], 32 * 1024**2 + 1),
            "row budget": edit_manifest(["outputs", "genres.jsonl", "rows"], 50001),
            "absent node expands": lambda d: mutate(d, "genre_parents.jsonl", parents + b'{"genre_qid":"Q107","parent_qid":"Q100"}\n'),
            "unknown schema": edit_manifest(["schema"], "nntune.genre-extract.v2"),
            "wrong licence": edit_manifest(["source", "license"], "CC-BY-NC-SA-4.0"),
            "missing final newline": lambda d: mutate(d, "genre_parents.jsonl", parents.rstrip(b"\n")),
            "unknown genre ref": lambda d: mutate(
                d, "artist_genres.jsonl", jsonl(members + [{"artist_mbid": D, "genre_qid": "Q999"}])),
            "unknown parent ref": lambda d: mutate(
                d, "genre_parents.jsonl", parents + b'{"genre_qid":"Q100","parent_qid":"Q998"}\n'),
            "duplicate row": lambda d: mutate(d, "artist_genres.jsonl", jsonl(members + [members[0]])),
            "duplicate genre": lambda d: mutate(d, "genres.jsonl", jsonl(genres + [genres[0]])),
            "malformed json": lambda d: mutate(d, "genre_parents.jsonl", b"{bad\n"),
            "duplicate json key": lambda d: mutate(
                d, "genre_parents.jsonl", b'{"genre_qid":"Q100","genre_qid":"Q101","parent_qid":"Q100"}\n'),
            "noncanonical mbid": lambda d: mutate(
                d, "artist_genres.jsonl", jsonl([{"artist_mbid": A.upper(), "genre_qid": "Q101"}])),
            "extra field": lambda d: mutate(
                d, "genres.jsonl", jsonl([dict(genres[0], extra=1)] + genres[1:])),
        }
        for label, damage in cases.items():
            with self.subTest(label):
                extract = self.extract_copy()
                damage(extract)
                self.assertFalse(self.refused(extract=extract).exists())

    def test_invalid_config_or_cohort_refused_without_output(self):
        def bad_config(change):
            cfg = config()
            change(cfg)
            path = self.fresh("config.json")
            write_json(path, cfg)
            return {"config_path": path}

        def bad_cohort(value):
            path = self.fresh("cohort.json")
            write_json(path, value)
            return {"cohort_path": path}

        good = cohort()
        cases = {
            "unknown override root": bad_config(lambda c: c["overrides"].append(
                {"genre_qid": "Q101", "action": "add", "roots": ["nope"], "reason": "x"})),
            "retired override root": bad_config(lambda c: c["overrides"].append(
                {"genre_qid": "Q101", "action": "add", "roots": ["test_old"], "reason": "x"})),
            "duplicate root": bad_config(lambda c: c["roots"].append(dict(c["roots"][0], sort_order=9, anchors=["Q7"]))),
            "unknown top key": bad_config(lambda c: c.update(extra=1)),
            "absent active anchors": bad_config(lambda c: c["roots"][0].update(anchors=["Q107"])),
            "hyphenated canonical root id": bad_config(lambda c: c["roots"].append(
                {"root_id": "test-root", "display_name": "x", "sort_order": 9, "anchors": ["Q101"]})),
            "playable not in artists": bad_cohort(cohort(artists=(A,), playable=(A, C))),
            "bad dataset kind": bad_cohort(dict(good, dataset_kind="production")),
            "bad cohort schema": bad_cohort(dict(good, schema="other")),
            "extra cohort key": bad_cohort(dict(good, extra=[])),
            "invalid mbid": bad_cohort(cohort(artists=("not-a-uuid",), playable=())),
            "nil mbid": bad_cohort(cohort(artists=("00000000-0000-0000-0000-000000000000",), playable=())),
        }
        for label, kwargs in cases.items():
            with self.subTest(label):
                self.assertFalse(self.refused(**kwargs).exists())

    def test_unsafe_output_refused_and_previous_directory_unchanged(self):
        previous = self.fresh("previous")
        previous.mkdir()
        (previous / "coverage.json").write_bytes(b"previous\n")
        (previous / "sentinel").write_bytes(b"keep\n")
        before = tree(previous)
        self.refused(output=previous)
        self.assertEqual(tree(previous), before)

        link_target = self.fresh("link-target")
        link_target.mkdir()
        link = self.fresh("link")
        link.symlink_to(link_target, target_is_directory=True)
        self.refused(output=link)
        self.assertEqual(list(link_target.iterdir()), [])

        occupied = self.fresh("occupied-file")
        occupied.write_bytes(b"file\n")
        self.refused(output=occupied)
        self.assertEqual(occupied.read_bytes(), b"file\n")

        orphan = self.fresh("missing-parent") / "child"
        self.refused(output=orphan)
        self.assertFalse(orphan.parent.exists())

    def test_symlinked_or_non_regular_input_refused(self):
        link = self.fresh("config-link.json")
        link.symlink_to(self.config_path)
        self.assertFalse(self.refused(config_path=link).exists())
        self.assertFalse(self.refused(cohort_path=self.root).exists())
        extract = self.extract_copy()
        (extract / "genres.jsonl").rename(extract / "real.jsonl")
        (extract / "genres.jsonl").symlink_to(extract / "real.jsonl")
        self.assertFalse(self.refused(extract=extract).exists())


if __name__ == "__main__":
    unittest.main()
