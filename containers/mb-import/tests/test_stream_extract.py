#!/usr/bin/env python3
"""stream_extract.py and the etl.py disk-bounded mode (R2 snapshot spike).

Run from the repository root: python3 containers/mb-import/tests/test_stream_extract.py
"""

import gzip
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXTRACT = ROOT / "stream_extract.py"


def load(name, path, env=None):
    saved = dict(os.environ)
    os.environ.update(env or {})
    try:
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        os.environ.clear()
        os.environ.update(saved)


def tar_bytes(members):
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w") as archive:
        for name, data in members.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


class StreamExtractTest(unittest.TestCase):
    def test_required_tables_match_the_etl_inputs(self):
        etl = load("etl", ROOT / "etl.py")
        extract = load("stream_extract", EXTRACT)
        self.assertEqual(set(extract.REQUIRED_TABLES), set(etl.FIELD_COUNTS))

    def run_extract(self, members):
        out = tempfile.mkdtemp()
        result = subprocess.run([sys.executable, str(EXTRACT), out], input=tar_bytes(members),
                                capture_output=True)
        return result, Path(out)

    def test_keeps_only_allowlisted_tables_gzipped(self):
        extract = load("stream_extract", EXTRACT)
        members = {f"mbdump/{t}": f"{t}\trow\n".encode() for t in extract.REQUIRED_TABLES}
        members["mbdump/tag"] = b"never kept\n"
        members["TIMESTAMP"] = b"2026-11-04 00:22:22+00\n"
        result, out = self.run_extract(members)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((out / "tag.gz").exists())
        self.assertEqual(gzip.decompress((out / "artist.gz").read_bytes()), b"artist\trow\n")
        meta = json.loads((out / "dump-meta.json").read_text())
        self.assertEqual(meta["meta"]["TIMESTAMP"], "2026-11-04 00:22:22+00")
        self.assertEqual(meta["skipped_members"], 1)
        self.assertEqual(meta["missing"], [])

    def test_missing_required_table_fails(self):
        result, out = self.run_extract({"mbdump/artist": b"x\n"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing required tables", result.stderr.decode())
        missing = json.loads((out / "dump-meta.json").read_text())["missing"]
        self.assertIn("track", missing)
        self.assertIn("artist_alias", missing)


class DuplicateMemberTest(unittest.TestCase):
    def test_duplicate_allowed_member_is_refused_before_opening_output(self):
        out = Path(tempfile.mkdtemp())
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w") as archive:
            for data in (b"first\n", b"second\n"):
                info = tarfile.TarInfo("mbdump/artist")
                info.size = len(data)
                archive.addfile(info, io.BytesIO(data))
        result = subprocess.run([sys.executable, str(EXTRACT), str(out)], input=buffer.getvalue(),
                                capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"duplicate", result.stderr)
        self.assertEqual(gzip.decompress((out / "artist.gz").read_bytes()), b"first\n")


class EtlDiskBoundedModeTest(unittest.TestCase):
    def test_plain_and_gz_for_one_table_is_refused(self):
        etl = load("etl_both", ROOT / "etl.py", {"MB_ETL_CONSUME_INPUTS": "1"})
        directory = Path(tempfile.mkdtemp())
        (directory / "area").write_bytes(b"1\t" + b"\t".join([b"x"] * 13) + b"\n")
        (directory / "area.gz").write_bytes(gzip.compress(b"2\t" + b"\t".join([b"y"] * 13) + b"\n"))
        with self.assertRaisesRegex(ValueError, "both"):
            list(etl.rows(directory / "area", 14))
        self.assertTrue((directory / "area").exists())
        self.assertTrue((directory / "area.gz").exists())

    def test_gz_inputs_are_read_and_consumed(self):
        etl = load("etl_consume", ROOT / "etl.py", {"MB_ETL_CONSUME_INPUTS": "1"})
        directory = Path(tempfile.mkdtemp())
        packed = directory / "area.gz"
        packed.write_bytes(gzip.compress(b"1\t" + b"\t".join([b"x"] * 13) + b"\n"))
        rows = list(etl.rows(directory / "area", 14))
        self.assertEqual(rows[0][0], "1")
        self.assertFalse(packed.exists())

    def test_plain_mode_is_unchanged(self):
        etl = load("etl_plain", ROOT / "etl.py")
        self.assertEqual(etl.curated_name("artist"), "mbdump-artist.tsv")
        directory = Path(tempfile.mkdtemp())
        with self.assertRaisesRegex(ValueError, "missing input file"):
            list(etl.rows(directory / "artist", 19))

    def test_gzip_output_is_reproducible(self):
        etl = load("etl_gz", ROOT / "etl.py", {"MB_ETL_GZIP_OUTPUT": "1"})
        self.assertEqual(etl.curated_name("artist"), "mbdump-artist.tsv.gz")
        outputs = []
        for _ in range(2):
            path = Path(tempfile.mkdtemp()) / "mbdump-artist.tsv.gz"
            with etl.open_output(path) as target:
                target.write("row\n")
            outputs.append(path.read_bytes())
        self.assertEqual(outputs[0], outputs[1])
        self.assertEqual(gzip.decompress(outputs[0]), b"row\n")


class ArtistAliasProjectionTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        source = ROOT / "tests" / "fixtures" / "mbdump"
        self.raw = shutil.copytree(source, self.tmp / "raw")
        self.aliases = self.raw / "artist_alias"
        self.curated = self.raw / "curated"
        self.output = "mbdump-artist_alias.tsv"

    def run_etl(self, directory, **mode):
        env = dict(os.environ, DUMP_TS="2025-02-03T04:05:06Z",
                   MB_ETL_CONSUME_INPUTS="0", MB_ETL_GZIP_OUTPUT="0")
        env.update(mode)
        return subprocess.run([sys.executable, str(ROOT / "etl.py"), str(directory)],
                              env=env, capture_output=True, text=True, check=False)

    def test_streamed_aliases_match_plain_after_reordering(self):
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        # The winner must come from the same source row, even with reversed ties.
        members["mbdump/artist_alias"] = b"".join(reversed(self.aliases.read_bytes().splitlines(keepends=True)))
        packed = self.tmp / "packed"
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        self.assertTrue((packed / "artist_alias.gz").is_file())
        gzip_etl = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(gzip_etl.returncode, 0, gzip_etl.stderr)
        self.assertFalse((packed / "artist_alias.gz").exists())
        self.assertEqual(gzip.decompress((packed / "curated" / (self.output + ".gz")).read_bytes()),
                         (self.curated / self.output).read_bytes())

    def test_empty_alias_table_emits_empty_sidecar(self):
        self.aliases.write_bytes(b"")
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), b"")

    def test_alias_errors_and_changed_output_preserve_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        original = self.aliases.read_bytes()
        for changed, error in (
            (None, "missing input file:"),
            (b"broken\n", "expected 16 fields"),
            (original.replace(rb"Caf\303\xA9", rb"Caf\303"), "invalid COPY text in name or locale"),
            (original.replace(b"11\t10\t", b"11\t999\t", 1), "artist_alias references missing artist 999"),
            (original.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            (original.replace(b"\tt\tf\n", b"\t\\N\tf\n", 1), "invalid primary_for_locale"),
            (original.replace(b"Jazz", b"Updated"), "already has different curated output"),
        ):
            with self.subTest(error=error):
                if changed is None:
                    self.aliases.unlink()
                else:
                    self.aliases.write_bytes(changed)
                refused = self.run_etl(self.raw)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)


if __name__ == "__main__":
    unittest.main()
