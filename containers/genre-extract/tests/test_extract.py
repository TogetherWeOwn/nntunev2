"""Black-box tests for containers/genre-extract/extract.py (stdlib only)."""

import bz2
import gzip
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EXTRACT = os.path.join(os.path.dirname(HERE), "extract.py")
FIXTURES = os.path.join(HERE, "fixtures")
GOLDEN = os.path.join(FIXTURES, "expected")
ROW_FILES = ["artist_genres.jsonl", "genre_parents.jsonl", "genres.jsonl"]
ALL_FILES = ROW_FILES + ["manifest.json"]
TIMEOUT = 60


def read_bytes(path):
    with open(path, "rb") as fh:
        return fh.read()


def golden_rows(name):
    return [json.loads(x) for x in read_bytes(os.path.join(GOLDEN, name)).decode().splitlines()]


def rows(directory, name):
    return [json.loads(x) for x in read_bytes(os.path.join(directory, name)).decode().splitlines()]


def entity_lines():
    text = read_bytes(os.path.join(FIXTURES, "synthetic.json")).decode()
    lines = text.splitlines()
    assert lines[0] == "[" and lines[-1] == "]"
    return [x.rstrip(",") for x in lines[1:-1]]


def as_array(lines):
    return ("[\n" + ",\n".join(lines) + "\n]\n").encode()


def as_jsonl(lines):
    return ("\n".join(lines) + "\n").encode()


class ExtractCase(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = tmp.name
        self.n = 0

    def write_input(self, data, name="in.json"):
        path = os.path.join(self.tmp, name)
        with open(path, "wb") as fh:
            fh.write(data)
        return path

    def run_cli(self, path, out=None, *extra, version="synthetic-v1"):
        self.n += 1
        if out is None:
            out = os.path.join(self.tmp, "out%d" % self.n)
        argv = [sys.executable, "-I", EXTRACT, "--input", path, "--output", out,
                "--dump-version", version, *extra]
        proc = subprocess.run(argv, capture_output=True, timeout=TIMEOUT)
        return proc, out

    def ok(self, path, *extra, out=None):
        proc, out = self.run_cli(path, out, *extra)
        self.assertEqual(proc.returncode, 0, proc.stderr.decode(errors="replace"))
        self.assertEqual(sorted(os.listdir(out)), sorted(ALL_FILES))
        return out

    def fails(self, path, *extra, out=None):
        proc, out = self.run_cli(path, out, *extra)
        self.assertNotEqual(proc.returncode, 0)
        self.assertFalse(os.path.exists(out), "output published on failure")
        self.assertEqual([x for x in os.listdir(self.tmp) if x.startswith(("out", ".genre-extract-"))],
                         [], "temp or partial sibling left behind")
        return proc

    def golden_input(self):
        return self.write_input(as_array(entity_lines()))


class GoldenTests(ExtractCase):
    def test_golden_rows(self):
        out = self.ok(self.golden_input())
        for name in ROW_FILES:
            self.assertEqual(rows(out, name), golden_rows(name), name)
            self.assertTrue(read_bytes(os.path.join(out, name)).endswith(b"\n"))

    def test_manifest(self):
        data = as_array(entity_lines())
        out = self.ok(self.write_input(data))
        m = json.loads(read_bytes(os.path.join(out, "manifest.json")))
        self.assertEqual(m["schema"], "nntune.genre-extract.v1")
        self.assertEqual(m["extractor_version"], "1")
        self.assertEqual(m["source"]["name"], "wikidata")
        self.assertEqual(m["source"]["license"], "CC0-1.0")
        self.assertEqual(m["source"]["dump_version"], "synthetic-v1")
        self.assertEqual(m["input"]["sha256"], hashlib.sha256(data).hexdigest())
        self.assertEqual(m["input"]["bytes"], len(data))
        self.assertEqual(m["input"]["format"], "json-array")
        self.assertEqual(m["language"], "en")
        self.assertIn("limits", m)
        self.assertIn("counts", m)
        self.assertEqual(sorted(m["outputs"]), sorted(ROW_FILES))
        for name in ROW_FILES:
            raw = read_bytes(os.path.join(out, name))
            self.assertEqual(m["outputs"][name], {
                "sha256": hashlib.sha256(raw).hexdigest(),
                "bytes": len(raw),
                "rows": len(raw.splitlines()),
            })
        text = read_bytes(os.path.join(out, "manifest.json")).decode()
        self.assertNotIn(self.tmp, text)

    def test_row_files_sorted(self):
        out = self.ok(self.golden_input())
        for name in ROW_FILES:
            lines = read_bytes(os.path.join(out, name)).splitlines()
            self.assertEqual(lines, sorted(lines), name)

    def test_negative_cases_excluded(self):
        out = self.ok(self.golden_input())
        mbids = {r["artist_mbid"] for r in rows(out, "artist_genres.jsonl")}
        self.assertEqual(len(mbids), 3)  # missing, malformed, no-P136 artists skipped
        genres = {r["genre_qid"] for r in rows(out, "genres.jsonl")}
        self.assertNotIn("Q108", genres)  # unused genre
        self.assertNotIn("Q999", genres)  # deprecated claim only
        self.assertEqual(len(rows(out, "artist_genres.jsonl")), 4)  # duplicates deduped
        for name in ROW_FILES:
            self.assertNotIn(b"rock", read_bytes(os.path.join(out, name)))

    def test_deterministic(self):
        path = self.golden_input()
        a = self.ok(path)
        b = self.ok(path)
        for name in ALL_FILES:
            self.assertEqual(read_bytes(os.path.join(a, name)), read_bytes(os.path.join(b, name)))

    def test_reordered_entities(self):
        lines = entity_lines()
        a = self.ok(self.write_input(as_array(lines), "a.json"))
        b = self.ok(self.write_input(as_array(list(reversed(lines))), "b.json"))
        for name in ROW_FILES:
            self.assertEqual(read_bytes(os.path.join(a, name)), read_bytes(os.path.join(b, name)))
        ma = json.loads(read_bytes(os.path.join(a, "manifest.json")))
        mb = json.loads(read_bytes(os.path.join(b, "manifest.json")))
        self.assertNotEqual(ma["input"]["sha256"], mb["input"]["sha256"])

    def test_formats_and_compression(self):
        lines = entity_lines()
        plain = as_array(lines)
        ref = self.ok(self.write_input(plain, "plain.json"))
        cases = [
            ("a.json.gz", gzip.compress(plain, mtime=0), ()),
            ("a.json.bz2", bz2.compress(plain), ()),
            ("a.jsonl", as_jsonl(lines), ("--format", "jsonl")),
            ("a.jsonl.gz", gzip.compress(as_jsonl(lines), mtime=0), ("--format", "jsonl")),
        ]
        for name, data, extra in cases:
            out = self.ok(self.write_input(data, name), *extra)
            for rf in ROW_FILES:
                self.assertEqual(read_bytes(os.path.join(out, rf)),
                                 read_bytes(os.path.join(ref, rf)), name)
        # Hash covers decompressed bytes, so compression does not change it.
        gz = self.ok(self.write_input(gzip.compress(plain, mtime=0), "b.json.gz"))
        self.assertEqual(read_bytes(os.path.join(gz, "manifest.json")),
                         read_bytes(os.path.join(ref, "manifest.json")))

    def test_existing_output_refused(self):
        out = os.path.join(self.tmp, "exists")
        os.mkdir(out)
        marker = os.path.join(out, "keep")
        open(marker, "w").close()
        proc, _ = self.run_cli(self.golden_input(), out)
        self.assertNotEqual(proc.returncode, 0)
        self.assertEqual(os.listdir(out), ["keep"])


class FailureTests(ExtractCase):
    def mutate(self, fn):
        ents = [json.loads(x) for x in entity_lines()]
        fn(ents)
        return self.write_input(as_array([json.dumps(x) for x in ents]))

    def test_malformed_json(self):
        lines = entity_lines()
        lines[2] = lines[2][:-5]
        self.fails(self.write_input(as_array(lines)))

    def test_bad_framing(self):
        lines = entity_lines()
        self.fails(self.write_input(("\n".join(lines) + "\n").encode()))  # no brackets
        self.fails(self.write_input(as_array(lines)[:-2]))  # missing closing delimiter
        self.fails(self.write_input(as_array(lines[:-1]).replace(b"\n]", b",\n]")))  # dangling comma

    def test_truncated_compressed(self):
        data = gzip.compress(as_array(entity_lines()), mtime=0)
        self.fails(self.write_input(data[: len(data) // 2], "t.json.gz"))
        data = bz2.compress(as_array(entity_lines()))
        self.fails(self.write_input(data[: len(data) // 2], "t.json.bz2"))

    def test_duplicate_entity_ids(self):
        lines = entity_lines()
        self.fails(self.write_input(as_array(lines + [lines[0]])))

    def test_structural_entity_errors(self):
        def bad_claims(e):
            e[0]["claims"] = []
        def bad_id(e):
            e[0]["id"] = "Q01"
        def bad_type(e):
            e[0]["type"] = "widget"
        def bad_claim_list(e):
            e[0]["claims"]["P136"] = {"x": 1}
        for fn in (bad_claims, bad_id, bad_type, bad_claim_list):
            with self.subTest(fn.__name__):
                self.fails(self.mutate(fn))

    def test_limits(self):
        path = self.golden_input()
        for extra in (("--max-line-bytes", "100"),
                      ("--max-input-bytes", "200"),
                      ("--max-entities", "3"),
                      ("--max-depth", "1"),
                      ("--max-db-bytes", "1"),
                      ("--max-output-bytes", "1")):
            with self.subTest(extra=extra):
                self.fails(path, *extra)

    def test_duplicate_keys_and_nonfinite(self):
        lines = entity_lines()
        for bad in ('{"type":"item","id":"Q1","id":"Q1"}',
                    '{"type":"item","id":"Q1","x":NaN}',
                    '{"type":"item","id":"Q1","x":Infinity}'):
            with self.subTest(bad=bad):
                self.fails(self.write_input(as_array(lines + [bad])))

    def test_bad_labels_and_utf8(self):
        for text in ("a\\u0001b", "a\\ud800b", "   "):
            line = '{"type":"item","id":"Q1","labels":{"en":{"language":"en","value":"%s"}}}' % text
            with self.subTest(text=text):
                self.fails(self.write_input(as_array([line])))
        self.fails(self.write_input(b'[\n{"type":"item","id":"Q1","x":"\xff"}\n]\n'))

    def test_db_saturation(self):
        lines = [json.dumps(item_entity("Q%d" % (1000 + i), "x" * 1024)) for i in range(300)]
        self.fails(self.write_input(as_jsonl(lines), "big.jsonl"), "--format", "jsonl",
                   "--max-db-bytes", "65536")

    def test_decompressed_input_limit(self):
        data = gzip.compress(as_array(entity_lines()), mtime=0)
        self.fails(self.write_input(data, "z.json.gz"), "--max-input-bytes", "1000")

    def test_missing_input(self):
        self.fails(os.path.join(self.tmp, "nope.json"))


def stmt(prop, value, **over):
    if prop == "P434":
        snak = {"snaktype": "value", "property": prop, "datatype": "external-id",
                "datavalue": {"type": "string", "value": value}}
    else:
        snak = {"snaktype": "value", "property": prop, "datatype": "wikibase-item",
                "datavalue": {"type": "wikibase-entityid",
                              "value": {"entity-type": "item", "id": value}}}
    st = {"mainsnak": snak, "type": "statement", "rank": "normal"}
    st.update(over)
    return st


def item_entity(qid, label=None, **claims):
    ent = {"type": "item", "id": qid, "claims": claims}
    if label:
        ent["labels"] = {"en": {"language": "en", "value": label}}
    return ent


def dumps(ents):
    return as_array([json.dumps(e) for e in ents]) if ents else b"[]\n"


class EdgeTests(ExtractCase):
    def manifest(self, out):
        return json.loads(read_bytes(os.path.join(out, "manifest.json")))

    def test_empty_inputs(self):
        for name, data, extra in (("e.json", b"[]\n", ()), ("e2.json", b"[\n]\n", ()),
                                  ("e.jsonl", b"", ("--format", "jsonl"))):
            with self.subTest(name=name):
                out = self.ok(self.write_input(data, name), *extra)
                for rf in ROW_FILES:
                    self.assertEqual(read_bytes(os.path.join(out, rf)), b"")
                self.assertEqual(self.manifest(out)["outputs"][ROW_FILES[0]]["rows"], 0)

    def test_invalid_numeric_id_claim_skipped(self):
        ents = [json.loads(x) for x in entity_lines()]
        ents[0]["claims"]["P136"][0]["mainsnak"]["datavalue"]["value"]["numeric-id"] = 7
        out = self.ok(self.write_input(dumps(ents)))
        genres = [r["genre_qid"] for r in rows(out, "artist_genres.jsonl")
                  if r["artist_mbid"].startswith("aaaa")]
        self.assertEqual(genres, ["Q102"])
        self.assertGreaterEqual(self.manifest(out)["counts"]["invalid_claims"], 1)

    def test_non_item_entities_skipped(self):
        ents = [item_entity("Q1", P434=[stmt("P434", "11111111-1111-4111-8111-111111111111")],
                            P136=[stmt("P136", "Q2")]),
                {"type": "property", "id": "P5", "claims": {"P136": [stmt("P136", "Q9")]}},
                {"type": "lexeme", "id": "L1"}]
        out = self.ok(self.write_input(dumps(ents)))
        self.assertEqual([r["genre_qid"] for r in rows(out, "genres.jsonl")], ["Q2"])

    def test_malformed_claim_shapes_skipped(self):
        good = stmt("P136", "Q2")
        bad = [
            {"type": "statement", "rank": "normal"},
            stmt("P136", "Q3", rank="bogus"),
            stmt("P136", "Q3", mainsnak={"snaktype": "value", "property": "P136",
                                         "datatype": "string", "datavalue": {}}),
            stmt("P136", "Q3", mainsnak={"snaktype": "value", "property": "P31",
                                         "datatype": "wikibase-item", "datavalue": {}}),
            stmt("P136", "Q3", mainsnak={"snaktype": "value", "property": "P136",
                                         "datatype": "wikibase-item", "datavalue": "x"}),
            stmt("P136", "Q03"),
            stmt("P136", "Q0"),
        ]
        ents = [item_entity("Q1", P434=[stmt("P434", "11111111-1111-4111-8111-111111111111"),
                                        stmt("P434", "zzz")],
                            P136=[good] + bad)]
        out = self.ok(self.write_input(dumps(ents)))
        self.assertEqual([r["genre_qid"] for r in rows(out, "artist_genres.jsonl")], ["Q2"])
        self.assertEqual([r["genre_qid"] for r in rows(out, "genres.jsonl")], ["Q2"])
        self.assertEqual(self.manifest(out)["counts"]["invalid_claims"], len(bad) + 1)

    def test_duplicate_and_multiple_mbids(self):
        u1, u2 = "ABCDEFAB-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222"
        ents = [item_entity("Q1", P434=[stmt("P434", u1), stmt("P434", u1.lower()), stmt("P434", u2)],
                            P136=[stmt("P136", "Q7"), stmt("P136", "Q7")])]
        out = self.ok(self.write_input(dumps(ents)))
        self.assertEqual(rows(out, "artist_genres.jsonl"), [
            {"artist_mbid": u2, "genre_qid": "Q7"},
            {"artist_mbid": u1.lower(), "genre_qid": "Q7"}])


if __name__ == "__main__":
    unittest.main()
