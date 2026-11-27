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
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

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

    def test_label_alias_requires_exact_archive_member(self):
        extract = load("stream_extract", EXTRACT)
        members = {f"mbdump/{table}": b"" for table in extract.REQUIRED_TABLES if table != "label_alias"}
        members["mbdump/nested/label_alias"] = b"not the core member\n"
        members["mbdump/label_alias_type"] = b"not the projection input\n"
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            result = subprocess.run([sys.executable, str(EXTRACT), str(out)],
                                    input=tar_bytes(members), capture_output=True, check=False)
            self.assertEqual(result.returncode, 1, result.stderr)
            meta = json.loads((out / "dump-meta.json").read_text())
            self.assertEqual(meta["missing"], ["label_alias"])
            self.assertEqual(meta["skipped_members"], 2)
            self.assertFalse((out / "label_alias.gz").exists())
            self.assertFalse((out / "label_alias_type.gz").exists())

    def test_missing_required_table_fails(self):
        result, out = self.run_extract({"mbdump/artist": b"x\n"})
        self.assertEqual(result.returncode, 1)
        self.assertIn("missing required tables", result.stderr.decode())
        missing = json.loads((out / "dump-meta.json").read_text())["missing"]
        self.assertIn("track", missing)
        self.assertIn("artist_alias", missing)
        self.assertIn("recording_alias", missing)
        self.assertIn("release_alias", missing)
        self.assertIn("release_group_alias", missing)
        self.assertIn("work", missing)
        self.assertIn("work_alias", missing)
        self.assertIn("work_type", missing)
        self.assertIn("l_recording_work", missing)
        self.assertTrue({"l_artist_recording", "l_artist_release", "l_artist_work"} <= set(missing))


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
        self.assertFalse((out / "artist.gz").exists())


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


class OfflineBoundaryTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.scratch = Path(directory.name)

    def test_copy_rows_reject_invalid_framing_and_oversize(self):
        etl = load("etl", ROOT / "etl.py", {"MB_ETL_CONSUME_INPUTS": "1"})
        for packed in (False, True):
            path = self.scratch / ("input.gz" if packed else "input")
            for data in (b"1\tmissing newline", b"1\tbare CR\r", b"1\t\0\n",
                         b"1\t" + b"x" * (1 << 20) + b"\n"):
                with self.subTest(packed=packed, prefix=data[:20]):
                    path.write_bytes(gzip.compress(data) if packed else data)
                    with self.assertRaises(ValueError):
                        list(etl.rows(self.scratch / "input", 2))
                    self.assertTrue(path.exists())
            path.unlink(missing_ok=True)

    def test_integer_fields_refuse_non_decimal_or_out_of_range_input(self):
        etl = load("etl", ROOT / "etl.py")
        for field in (" 1", "1_0", "１", "2147483648", "-2147483649"):
            with self.subTest(field=field), self.assertRaises(ValueError):
                etl.required_int(field, "test field")
        self.assertEqual(etl.required_int("-2147483648", "test field"), -2147483648)

    def test_archive_refuses_malformed_members_and_tail(self):
        extract = load("stream_extract", EXTRACT)
        complete = tar_bytes({f"mbdump/{table}": b"" for table in extract.REQUIRED_TABLES})
        block = tarfile.BLOCKSIZE
        body = complete[:len(extract.REQUIRED_TABLES) * block]
        oversized = tarfile.TarInfo("mbdump/artist")
        oversized.size = 1 << 48
        link = tarfile.TarInfo("TIMESTAMP")
        link.type = tarfile.SYMTYPE
        for malformed, reason in (
            (tar_bytes({"../artist": b""}), b"unsafe path"),
            (link.tobuf() + complete, b"unsupported member type"),
            (oversized.tobuf(tarfile.GNU_FORMAT), b"archive size limit"),
            (tar_bytes({"TIMESTAMP": b"x" * 4097}), b"metadata size limit"),
            (tar_bytes({"TIMESTAMP": b"\xff"}), b"invalid metadata"),
            (tar_bytes({"TIMESTAMP": b"first", "./TIMESTAMP": b"second"}), b"duplicate member"),
            (body + b"\0" * block, b"truncated archive"),
            (body + b"x" * block + complete[len(body):], b"invalid header"),
            (complete + b"extra", b"archive trailer"),
        ):
            with self.subTest(reason=reason), tempfile.TemporaryDirectory() as out:
                result = subprocess.run([sys.executable, str(EXTRACT), out], input=malformed,
                                        capture_output=True, check=False)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertIn(reason, result.stderr)
                self.assertFalse((Path(out) / "dump-meta.json").exists())
                self.assertFalse(list(Path(out).glob("*.gz")))

    def test_extract_refuses_to_overwrite_preexisting_table(self):
        outside = self.scratch / "outside"
        outside.write_bytes(b"keep me")
        out = self.scratch / "dump"
        out.mkdir()
        (out / "artist.gz").symlink_to(outside)
        result = subprocess.run([sys.executable, str(EXTRACT), str(out)],
                                input=tar_bytes({"mbdump/artist": b"new"}), capture_output=True, check=False)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(outside.read_bytes(), b"keep me")

    def test_readiness_requires_bounded_successful_typed_snapshot(self):
        readiness = load("readiness", ROOT / "check-readiness.py")
        mbid = "11111111-1111-4111-8111-111111111111"
        tracks = [[1, 1, mbid]]
        evidence = self.scratch / "evidence.json"
        evidence.write_text(json.dumps({
            "tables": {table: {"sha256": "0" * 64, "lines": 1, "rows": 1} for table in readiness.TABLES},
            "probes": [{"query": "example", "release_mbid": mbid, "artist_mbid": mbid, "tracks": tracks}],
        }))
        payload = self.scratch / "payload"
        psql = self.scratch / "psql"
        psql.write_text('#!/bin/sh\ncat "$TEST_SNAPSHOT"\nexit "$TEST_STATUS"\n')
        psql.chmod(0o700)
        counts = b"[1,1,0]\n" * len(readiness.TABLES)
        hits = json.dumps([[mbid, mbid, True]]).encode() + b"\n"
        recordings = json.dumps(tracks).encode() + b"\n"
        good = counts + hits + recordings
        for raw, status, deadline, expected in (
            (good, 0, 10, 0),
            (good, 1, 10, 2),
            (good, 0, 0, 2),
            (good.replace(b"[1,1,0]", b"[true,1,0]", 1), 0, 10, 2),
            (good.replace(b"true", b"1"), 0, 10, 2),
            (good.replace(b"[[1", b"[[true"), 0, 10, 2),
            (counts + hits + b" " * (512 * 1024) + recordings, 0, 10, 2),
            (good + b"[]\n", 0, 10, 2),
        ):
            with self.subTest(size=len(raw), prefix=raw[:12], status=status, deadline=deadline):
                payload.write_bytes(raw)
                with patch.dict(os.environ, PATH=f"{self.scratch}:{os.environ['PATH']}",
                                TEST_SNAPSHOT=str(payload), TEST_STATUS=str(status)), \
                        patch.object(readiness, "PSQL_TIMEOUT", deadline, create=True), \
                        patch.object(sys, "argv", ["check-readiness.py", str(evidence)]), \
                        patch("sys.stdout", new_callable=io.StringIO), patch("sys.stderr", new_callable=io.StringIO):
                    try:
                        result = readiness.main()
                    except SystemExit as exc:
                        result = exc.code
                    self.assertEqual(result, expected)


class ProjectionTestCase(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.tmp = Path(tmp.name)
        source = ROOT / "tests" / "fixtures" / "mbdump"
        self.raw = shutil.copytree(source, self.tmp / "raw")
        self.curated = self.raw / "curated"

    def run_etl(self, directory, **mode):
        env = dict(os.environ, DUMP_TS="2025-02-03T04:05:06Z",
                   MB_ETL_CONSUME_INPUTS="0", MB_ETL_GZIP_OUTPUT="0")
        env.update(mode)
        return subprocess.run([sys.executable, str(ROOT / "etl.py"), str(directory)],
                              env=env, capture_output=True, text=True, check=False)


class ArtistAliasProjectionTest(ProjectionTestCase):
    output = "mbdump-artist_alias.tsv"

    def setUp(self):
        super().setUp()
        self.aliases = self.raw / "artist_alias"

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


class RecordingAliasProjectionTest(ProjectionTestCase):
    output = "mbdump-recording_alias.tsv"

    def test_decoded_keys_and_raw_winners_match_streamed_gzip(self):
        # Both English aliases survive: echo reprise sorts before Écho only
        # after decoding the latter's escaped COPY spelling.
        self.assert_alias_output_matches_in_both_modes()

    def test_conflicting_primaries_break_ties_by_sort_name_then_id(self):
        aliases = self.raw / "recording_alias"
        rows = aliases.read_bytes().splitlines(keepends=True)
        # MusicBrainz permits only one primary per non-null locale. Exercise
        # the curator's fallback ordering separately from the normal fixture.
        for index, row in enumerate(rows):
            if row.startswith(b"708\t"):
                # Echo\101 ranks below Echo\tMix raw, but above it decoded.
                row = row.replace(b"\tAlternate\t", b"\tEcho\\101\t", 1)
            if row.startswith((b"708\t", b"710\t")):
                rows[index] = row.removesuffix(b"f\tf\n") + b"t\tf\n"
        aliases.write_bytes(b"".join(rows))
        self.assert_alias_output_matches_in_both_modes()

    def test_alias_id_breaks_ties_independently_of_rowid(self):
        etl = load("etl_alias_order", ROOT / "etl.py", {"MB_ETL_GZIP_OUTPUT": "0"})
        db = sqlite3.connect(":memory:")
        self.addCleanup(db.close)
        # An INTEGER PRIMARY KEY aliases rowid and masks a missing ID sort.
        db.executescript(r"""
            CREATE TABLE recording (id INTEGER PRIMARY KEY, gid TEXT);
            INSERT INTO recording VALUES (50, 'dddddddd-dddd-4ddd-8ddd-dddddddddddd');
            CREATE TABLE recording_alias (
                id INTEGER NOT NULL, entity INTEGER, name TEXT, locale TEXT,
                sort_name TEXT, primary_flag TEXT, name_key TEXT, locale_key TEXT
            );
            INSERT INTO recording_alias VALUES
                (710, 50, 'Echo', 'en', 'Echo', 't', 'Echo', 'en'),
                (709, 50, '\105cho', 'en', 'Echo', 't', 'Echo', 'en');
        """)
        stamp = "2025-02-03T04:05:06Z"
        self.assertEqual(etl.project_aliases(db, "recording", self.tmp, stamp), 1)
        self.assertEqual((self.tmp / self.output).read_text(encoding="utf-8"),
                         f"dddddddd-dddd-4ddd-8ddd-dddddddddddd\t\\105cho\ten\tEcho\tt\t{stamp}\n")

    def assert_alias_output_matches_in_both_modes(self):
        expected = (ROOT / "tests" / "expected" / self.output).read_bytes()
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), expected)

        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        key = "mbdump/recording_alias"
        members[key] = b"".join(reversed(members[key].splitlines(keepends=True)))
        members["mbdump/recording_tag"] = b"not part of the CC0 core projection\n"
        packed = self.tmp / "packed"
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        self.assertTrue((packed / "recording_alias.gz").is_file())
        self.assertFalse((packed / "recording_tag.gz").exists())
        gzip_etl = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(gzip_etl.returncode, 0, gzip_etl.stderr)
        self.assertFalse((packed / "recording_alias.gz").exists())
        self.assertEqual(gzip.decompress((packed / "curated" / (self.output + ".gz")).read_bytes()), expected)

    def test_empty_recording_alias_table_emits_empty_sidecar(self):
        (self.raw / "recording_alias").write_bytes(b"")
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), b"")

    def test_recording_alias_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        aliases = (self.raw / "recording_alias").read_bytes()
        recordings = (self.raw / "recording").read_bytes()
        for table, changed, error in (
            ("recording_alias", None, "missing input file:"),
            ("recording_alias", b"broken\n", "expected 16 fields"),
            ("recording_alias", aliases + aliases.splitlines(keepends=True)[0], "UNIQUE constraint failed"),
            ("recording_alias", aliases.replace(b"710\t50\t", b"2147483648\t50\t", 1), "invalid id"),
            ("recording_alias", aliases.replace(b"710\t50\t", b"710\t+50\t", 1), "invalid recording"),
            ("recording_alias", aliases.replace(b"710\t50\t", b"710\t999\t", 1), "references missing recording 999"),
            ("recording_alias", aliases.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            ("recording_alias", aliases.replace(rb"\303\x89cho", rb"\303cho"), "invalid COPY text in name or locale"),
            ("recording_alias", aliases.replace(b"\ten\t", b"\t\\000\t", 1), "invalid COPY text in name or locale"),
            ("recording_alias", aliases.replace(rb"Echo\tMix", rb"Echo\377", 1), "invalid COPY text in sort_name"),
            ("recording_alias", aliases.replace(rb"Echo\tMix", rb"\N", 1), "sort_name must not be NULL"),
            ("recording", recordings.replace(b"\n51\t", b"\n+51\t"), "references invalid recording id 51"),
            ("recording", recordings.replace(b"DDDDDDDD-DDDD-4DDD-8DDD-DDDDDDDDDDDD", b"bad-mbid"), "invalid recording MBID"),
        ):
            with self.subTest(table=table, error=error):
                path = self.raw / table
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-03T04:05:07Z")
                path.write_bytes(aliases if table == "recording_alias" else recordings)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)


class AliasProjectionTestCase(ProjectionTestCase):
    @property
    def output(self):
        return f"mbdump-{self.entity}_alias.tsv"

    def curated_mbids(self, table):
        return {row.split(b"\t", 1)[0] for row in
                (self.curated / f"mbdump-{table}.tsv").read_bytes().splitlines()}

    def assert_plain_and_streamed_output(self, expected):
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), expected)

        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        alias_table, excluded_table = f"{self.entity}_alias", f"{self.entity}_tag"
        key = f"mbdump/{alias_table}"
        members[key] = b"".join(reversed(members[key].splitlines(keepends=True)))
        members[f"mbdump/{excluded_table}"] = b"not part of the CC0 core projection\n"
        packed = self.tmp / "packed"
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        alias_gzip = packed / f"{alias_table}.gz"
        self.assertEqual(gzip.decompress(alias_gzip.read_bytes()), members[key])
        self.assertFalse((packed / f"{excluded_table}.gz").exists())
        gzip_etl = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(gzip_etl.returncode, 0, gzip_etl.stderr)
        self.assertFalse(alias_gzip.exists())
        self.assertEqual(gzip.decompress((packed / "curated" / (self.output + ".gz")).read_bytes()), expected)


class ReleaseAliasProjectionTest(AliasProjectionTestCase):
    entity = "release"

    def test_decoded_keys_and_raw_winners_match_streamed_gzip(self):
        # English tests primary preference and preserves two distinct names;
        # French tests raw sort-name order, then lowest ID for tied rows.
        self.assert_plain_and_streamed_output((ROOT / "tests" / "expected" / self.output).read_bytes())
        self.assertEqual(self.curated_mbids("release_alias"), self.curated_mbids("release"))

    def test_empty_release_alias_table_emits_empty_sidecar(self):
        (self.raw / "release_alias").write_bytes(b"")
        self.assert_plain_and_streamed_output(b"")

    def test_release_alias_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        alias_file = self.raw / "release_alias"
        aliases = alias_file.read_bytes()
        release_file = self.raw / "release"
        releases = release_file.read_bytes()
        for path, changed, error in (
            (alias_file, b"broken\n", "expected 16 fields"),
            (alias_file, aliases + aliases.splitlines(keepends=True)[0], "UNIQUE constraint failed: release_alias.id"),
            (alias_file, aliases.replace(b"810\t30\t", b"810\t+30\t", 1), "invalid release (expected"),
            (alias_file, aliases.replace(b"810\t30\t", b"810\t999\t", 1), "release_alias references missing release 999"),
            (alias_file, aliases.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            (alias_file, aliases.replace(rb"\303\x89dition", rb"\303dition", 1), "invalid COPY text in name or locale"),
            (alias_file, aliases.replace(rb"Edition\tBox", rb"Edition\377", 1), "invalid COPY text in sort_name"),
            (release_file, releases.replace(b"BBBBBBBB-BBBB-4BBB-8BBB-BBBBBBBBBBBB", b"bad-mbid"), "release_alias: invalid release MBID"),
        ):
            with self.subTest(error=error):
                path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-03T04:05:07Z")
                path.write_bytes(aliases if path == alias_file else releases)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class ReleaseGroupAliasProjectionTest(AliasProjectionTestCase):
    entity = "release_group"

    def test_decoded_keys_and_raw_winners_match_streamed_gzip(self):
        self.assert_plain_and_streamed_output((ROOT / "tests" / "expected" / self.output).read_bytes())
        alias_mbids = self.curated_mbids("release_group_alias")
        self.assertLess(alias_mbids, self.curated_mbids("release_group"))
        self.assertTrue(alias_mbids.isdisjoint(self.curated_mbids("release")))

    def test_empty_release_group_alias_table_emits_empty_sidecar(self):
        (self.raw / "release_group_alias").write_bytes(b"")
        self.assert_plain_and_streamed_output(b"")

    def test_release_group_alias_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        alias_file = self.raw / "release_group_alias"
        aliases = alias_file.read_bytes()
        group_file = self.raw / "release_group"
        groups = group_file.read_bytes()
        for path, changed, error in (
            (alias_file, b"broken\n", "expected 16 fields"),
            # 31 exists as a release, not as a release group.
            (alias_file, aliases.replace(b"910\t41\t", b"910\t31\t", 1),
             "release_group_alias references missing release_group 31"),
            (alias_file, aliases.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            (alias_file, aliases.replace(rb"\303\x89cho", rb"\303cho", 1), "invalid COPY text in name or locale"),
            (group_file, groups.replace(b"CCCCCCCC-CCCC-4CCC-8CCC-CCCCCCCCCCCC", b"bad-mbid"),
             "release_group_alias: invalid release_group MBID"),
        ):
            with self.subTest(error=error):
                path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-03T04:05:07Z")
                path.write_bytes(aliases if path == alias_file else groups)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class WorkAliasProjectionTest(AliasProjectionTestCase):
    entity = "work"

    def test_decoded_keys_and_raw_winners_match_streamed_gzip(self):
        self.assert_plain_and_streamed_output((ROOT / "tests" / "expected" / self.output).read_bytes())
        alias_mbids = self.curated_mbids("work_alias")
        self.assertLess(alias_mbids, self.curated_mbids("work"))
        self.assertTrue(alias_mbids.isdisjoint(self.curated_mbids("recording")))

    def test_empty_work_alias_table_emits_empty_sidecar(self):
        (self.raw / "work_alias").write_bytes(b"")
        self.assert_plain_and_streamed_output(b"")

    def test_work_alias_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        self.assertIn("work_alias=3,", published.stdout)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        path = self.raw / "work_alias"
        aliases = path.read_bytes()
        first_row = aliases.splitlines(keepends=True)[0]
        for changed, error in (
            (None, "missing input file:"),
            (b"broken\n", "expected 16 fields"),
            (aliases + first_row, "UNIQUE constraint failed: work_alias.id"),
            # 51 exists as a recording, not as a work. Keep valid aliases too.
            (aliases.replace(b"920\t80\t", b"920\t51\t", 1), "work_alias references missing work 51"),
            (aliases.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            (aliases.replace(rb"\303\x89tude", rb"\303tude", 1), "invalid COPY text in name or locale"),
            (aliases.replace(rb"Study\tOne", rb"Study\377", 1), "invalid COPY text in sort_name"),
        ):
            with self.subTest(error=error):
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-03T04:05:07Z")
                path.write_bytes(aliases)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class WorkProjectionTest(ProjectionTestCase):
    output = "mbdump-work.tsv"

    def test_typed_and_untyped_works_match_streamed_gzip(self):
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        expected = (ROOT / "tests" / "expected" / self.output).read_bytes()
        self.assertEqual((self.curated / self.output).read_bytes(), expected)

        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        members["mbdump/work"] = b"".join(reversed(members["mbdump/work"].splitlines(keepends=True)))
        excluded_tables = ("work_alias_type", "l_artist_label")
        for table in excluded_tables:
            members[f"mbdump/{table}"] = b"not admitted\n"
        packed = self.tmp / "packed"
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        for table in excluded_tables:
            self.assertFalse((packed / f"{table}.gz").exists())
        gzip_etl = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(gzip_etl.returncode, 0, gzip_etl.stderr)
        for table in ("work", "work_type"):
            self.assertFalse((packed / f"{table}.gz").exists())
        self.assertEqual(gzip.decompress((packed / "curated" / (self.output + ".gz")).read_bytes()), expected)

    def test_empty_work_table_emits_empty_sidecar(self):
        (self.raw / "work").write_bytes(b"")
        (self.raw / "work_alias").write_bytes(b"")
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), b"")

    def test_work_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        work = (self.raw / "work").read_bytes()
        lookup = (self.raw / "work_type").read_bytes()
        for table, changed, error in (
            ("work", None, "missing input file:"),
            ("work_type", None, "missing input file:"),
            ("work", b"broken\n", "expected 7 fields"),
            ("work_type", b"broken\n", "expected 6 fields"),
            ("work", work.replace(b"\t1\t", b"\t999\t"), "work 80 references missing work_type 999"),
            ("work", work.replace(b"\t1\t", b"\t2147483648\t"), "invalid type_id"),
            ("work", work.replace(b"81\t", b"\\N\t"), "invalid id"),
            ("work", work.replace(b"81\t", b"80\t"), "UNIQUE constraint failed: work.id"),
            ("work", work.replace(b"10101010-1010-4010-8010-101010101010", b"bad"), "invalid work MBID"),
            ("work", work.replace(b"10101010-1010-4010-8010-101010101010", b"\\N"), "invalid work MBID"),
            ("work", work.replace(b"10101010-1010-4010-8010-101010101010", b"80808080-8080-4808-8808-808080808080"),
             "UNIQUE constraint failed: work.gid"),
            ("work", work.replace(rb"A\tSong\\N", rb"\N"), "name must not be NULL"),
            ("work", work.replace(rb"A\tSong\\N", rb"A\q"), "invalid COPY text in name"),
            ("work_type", lookup.replace(b"1\t", b"+1\t", 1), "invalid id"),
            ("work_type", lookup * 2, "UNIQUE constraint failed: work_type.id"),
            ("work_type", lookup.replace(rb"Song\tSuite", rb"\000"), "invalid COPY text in name"),
            ("work", work.replace(b"Live", b"Studio"), "already has different curated output"),
        ):
            with self.subTest(table=table, error=error):
                path = self.raw / table
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                # Use a fresh timestamp except when testing the reuse check.
                mode = {} if error.startswith("already has") else {"DUMP_TS": "2025-02-04T04:05:06Z"}
                refused = self.run_etl(self.raw, **mode)
                path.write_bytes(work if table == "work" else lookup)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class RecordingWorkProjectionTest(ProjectionTestCase):
    output = "mbdump-recording_work.tsv"

    def setUp(self):
        super().setUp()
        self.edges = self.raw / "l_recording_work"
        timestamp = b"2025-01-01 00:00:00+00"
        with (self.raw / "link_type").open("ab") as types:
            types.write(b"513\t\\N\t0\taaaaaaaa-0006-4006-8006-000000000006\trecording\twork\tperformance"
                        b"\tPerformance\tPerformance\tPerformance\tPerformance\t" + timestamp + b"\tf\tt\t0\t0\n")
            types.write(b"514\t\\N\t0\taaaaaaaa-0007-4007-8007-000000000007\trecording\twork\tmedley"
                        b"\tMedley\tMedley\tMedley\tMedley\t" + timestamp + b"\tf\tt\t0\t0\n")
        with (self.raw / "link").open("ab") as links:
            for prefix in (b"216\t513", b"217\t513", b"218\t514"):
                links.write(prefix + b"\t\\N" * 6 + b"\t0\t" + timestamp + b"\tf\n")
        self.edges.write_bytes(b"".join(
            prefix + b"\t0\t" + timestamp + b"\t0\t\\N\t\\N\n" for prefix in (
                b"401\t216\t51\t80", b"402\t217\t50\t80", b"403\t218\t50\t80",
                b"404\t216\t50\t80", b"405\t216\t50\t81",
            )
        ))
        # Referencing 216 above must not admit it to the URL slice.
        with (self.raw / "l_recording_url").open("ab") as relations:
            relations.write(b"499\t216\t50\t999\t0\t" + timestamp + b"\t0\t\\N\t\\N\n")

    def run_streamed_etl(self):
        packed = self.tmp / "packed"
        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        self.assertTrue((packed / "l_recording_work.gz").is_file())
        result = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((packed / "l_recording_work.gz").exists())
        return gzip.decompress((packed / "curated" / (self.output + ".gz")).read_bytes())

    def test_directed_typed_triples_are_deduplicated_and_match_streamed_gzip(self):
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        expected = (ROOT / "tests" / "expected" / self.output).read_bytes()
        self.assertEqual((self.curated / self.output).read_bytes(), expected)
        for name in ("mbdump-recording_url.tsv", "mbdump-recording_music_video.tsv"):
            self.assertEqual((self.curated / name).read_bytes(), (ROOT / "tests" / "expected" / name).read_bytes())
        self.edges.write_bytes(b"".join(reversed(self.edges.read_bytes().splitlines(keepends=True))))
        self.assertEqual(self.run_streamed_etl(), expected)

    def test_equivalent_copy_type_names_collapse_in_decoded_order(self):
        types = self.raw / "link_type"
        performance = types.read_bytes().splitlines(keepends=True)[-2]
        with types.open("ab") as stream:
            stream.write(performance.replace(b"513\t", b"515\t", 1).replace(
                b"0006-4006-8006-000000000006", b"0008-4008-8008-000000000008",
            ).replace(b"\tperformance\t", b"\t\\160erformance\t"))
        link = self.raw / "link"
        link.write_bytes(link.read_bytes().replace(b"217\t513", b"217\t515"))
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        plain = (self.curated / self.output).read_bytes()
        self.assertEqual([row.split(b"\t")[2] for row in plain.splitlines()],
                         [b"performance", b"medley", b"\\160erformance", b"performance"])
        self.edges.write_bytes(b"".join(reversed(self.edges.read_bytes().splitlines(keepends=True))))
        self.assertEqual(self.run_streamed_etl(), plain)

    def test_empty_table_emits_empty_sidecar_in_both_modes(self):
        self.edges.write_bytes(b"")
        # Keep these keys unreferenced by the other strict projections.
        for name in ("recording_alias", "l_recording_recording"):
            (self.raw / name).write_bytes(b"")
        # Unreferenced keys retain the legacy loaders' acceptance rules.
        for table, key in (("recording", b"50"), ("link_type", b"504")):
            path = self.raw / table
            path.write_bytes(path.read_bytes().replace(key + b"\t", b"+" + key + b"\t", 1))
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), b"")
        self.assertEqual(self.run_streamed_etl(), b"")

    def test_invalid_references_and_fields_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        edges = self.edges.read_bytes()
        links = (self.raw / "link").read_bytes()
        types = (self.raw / "link_type").read_bytes()
        recordings = (self.raw / "recording").read_bytes()
        for table, changed, error in (
            (self.edges.name, None, "missing input file:"),
            (self.edges.name, b"broken\n", "expected 9 fields"),
            (self.edges.name, edges.replace(b"401\t216", b"401\t999"), "references missing link 999"),
            (self.edges.name, edges.replace(b"401\t216\t51", b"401\t216\t999"), "references missing recording 999"),
            (self.edges.name, edges.replace(b"401\t216\t51\t80", b"401\t216\t51\t999"), "references missing work 999"),
            (self.edges.name, edges.replace(b"401\t216", b"401\t\\N"), "invalid link"),
            (self.edges.name, edges.replace(b"401\t216\t51", b"401\t216\t+51"), "invalid recording"),
            (self.edges.name, edges.replace(b"401\t216\t51\t80", b"401\t216\t51\t2147483648"), "invalid work"),
            ("link", links.replace(b"216\t513", b"216\t999"), "references missing link_type 999"),
            ("link", links.replace(b"216\t513", b"216\t506"), "link_type 506 is not recording-to-work"),
            ("link", links.replace(b"216\t513", b"216\t+513"), "invalid link_type"),
            ("link", links + links.splitlines(keepends=True)[-1], "UNIQUE constraint failed: entity_relation_link.id"),
            ("link_type", types.replace(b"\n513\t", b"\n+513\t"), "invalid link_type id 513"),
            ("link_type", types.replace(b"\trecording\twork\tperformance", b"\t\\N\twork\tperformance"),
             "link_type 513 is not recording-to-work"),
            ("recording", recordings.replace(b"\n51\t", b"\n+51\t"), "invalid recording id 51"),
            ("link_type", types.replace(b"\tperformance\t", b"\t\t"), "name must not be empty"),
            ("link_type", types.replace(b"\tperformance\t", b"\t\\q\t"), "invalid COPY text in name"),
            ("recording", recordings.replace(b"DDDDDDDD-DDDD-4DDD-8DDD-DDDDDDDDDDDD", b"bad"), "invalid recording MBID"),
            (self.edges.name, b"", "already has different curated output"),
        ):
            with self.subTest(table=table, error=error):
                path = self.raw / table
                original = path.read_bytes()
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                mode = {} if error.startswith("already has") else {"DUMP_TS": "2025-02-04T04:05:06Z"}
                refused = self.run_etl(self.raw, **mode)
                path.write_bytes(original)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class RecordingRecordingProjectionTest(ProjectionTestCase):
    source = "l_recording_recording"
    output = "mbdump-recording_recording.tsv"
    video = "mbdump-recording_music_video.tsv"

    def pack(self, directory):
        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        result = subprocess.run([sys.executable, str(EXTRACT), str(directory)],
                                input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((directory / (self.source + ".gz")).is_file())

    def test_directed_deduplicated_edges_leave_legacy_video_bytes_unchanged(self):
        recording = (self.raw / "recording").read_bytes().splitlines(keepends=True)[0]
        third_mbid = b"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        with (self.raw / "recording").open("ab") as out:
            out.write(b"52\t" + third_mbid + b"\t" + recording.split(b"\t", 2)[2])
        links = self.raw / "link"
        types = self.raw / "link_type"
        type_row = types.read_bytes().splitlines(keepends=True)[-1]
        link_row = links.read_bytes().splitlines(keepends=True)[-1]
        with types.open("ab") as out:
            for offset, name in ((1, b"\\143over"), (2, b"alternate take")):
                out.write(type_row.replace(b"512\t", str(512 + offset).encode() + b"\t", 1)
                          .replace(b"000000000005", f"{5 + offset:012d}".encode())
                          .replace(b"\tcover\t", b"\t" + name + b"\t"))
        with links.open("ab") as out:
            out.write(link_row.replace(b"215\t512", b"216\t513", 1))
            out.write(link_row.replace(b"215\t512", b"217\t514", 1))
        edges = self.raw / self.source
        row = edges.read_bytes().splitlines(keepends=True)[-2]
        with edges.open("ab") as out:
            out.write(row.replace(b"323\t214", b"325\t216", 1))  # Equivalent type name.
            out.write(row.replace(b"323\t214", b"327\t217", 1))  # Distinct type, same pair.
            out.write(row.replace(b"323\t214\t50\t51", b"326\t214\t50\t52", 1))
        a = b"88888888-8888-4888-8888-888888888888"
        b = b"DDDDDDDD-DDDD-4DDD-8DDD-DDDDDDDDDDDD"
        tail = b"\t2025-02-03T04:05:06Z\n"
        # Decoded 'alternate take' sorts before 'cover', unlike the raw COPY spellings.
        expected = (a + b"\t" + b + b"\talternate take" + tail + a + b"\t" + b + b"\t\\143over" + tail
                    + a + b"\t" + third_mbid + b"\tcover" + tail + b + b"\t" + a + b"\tcover" + tail)
        video = (ROOT / "tests" / "expected" / self.video).read_bytes()
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / self.output).read_bytes(), expected)
        self.assertEqual((self.curated / self.video).read_bytes(), video)
        edges.write_bytes(b"".join(reversed(edges.read_bytes().splitlines(keepends=True))))
        packed = self.tmp / "packed"
        self.pack(packed)
        result = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse((packed / (self.source + ".gz")).exists())
        for name, contents in ((self.output, expected), (self.video, video)):
            self.assertEqual(gzip.decompress((packed / "curated" / (name + ".gz")).read_bytes()), contents)

    def test_empty_and_video_only_inputs_emit_empty_song_sidecar(self):
        edges = self.raw / self.source
        cases = (b"", b"".join(edges.read_bytes().splitlines(keepends=True)[:3]))
        for i, rows in enumerate(cases):
            with self.subTest(video_only=bool(rows)):
                edges.write_bytes(rows)
                packed = self.tmp / f"packed-{i}"
                self.pack(packed)
                for directory, mode in ((self.raw, False), (packed, True)):
                    result = self.run_etl(directory, MB_ETL_GZIP_OUTPUT=str(int(mode)),
                                          DUMP_TS=f"2025-02-03T04:05:0{6+i}Z")
                    self.assertEqual(result.returncode, 0, result.stderr)
                    if mode:
                        self.assertEqual(gzip.decompress((directory / "curated" / (self.output + ".gz")).read_bytes()), b"")
                    else:
                        self.assertEqual((self.curated / self.output).read_bytes(), b"")

    def test_invalid_references_refuse_without_changing_published_snapshot(self):
        # Recording 52 is referenced only by a video edge, never by an alias
        # or a song relation that could mask missing video validation.
        video_mbid = b"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        recording = (self.raw / "recording").read_bytes().splitlines(keepends=True)[-1]
        with (self.raw / "recording").open("ab") as out:
            out.write(b"52\t" + video_mbid + b"\t" + recording.split(b"\t", 2)[2])
        edges = self.raw / self.source
        edges.write_bytes(edges.read_bytes().replace(b"320\t205\t50\t51\t", b"320\t205\t50\t52\t", 1))
        packed = self.tmp / "packed"
        self.pack(packed)
        cases = (
            (self.source, b"323\t214", b"323\t999", "references missing link 999"),
            (self.source, b"323\t214\t50\t51", b"323\t214\t50\t999", "references missing recording (50, 999)"),
            (self.source, b"323\t214\t50", b"323\t214\t+50", "l_recording_recording row 4: invalid recording"),
            ("link", b"214\t512", b"214\t999", "references missing link_type 999"),
            ("link", b"214\t512", b"214\t504", "not recording-to-recording"),
            ("link_type", b"\tcover\t", b"\t\\q\t", "invalid COPY text in name"),
            ("recording", video_mbid, b"bad", "l_recording_recording: invalid recording MBID"),
        )
        for directory, compress in ((self.raw, False), (packed, True)):
            mode = {"MB_ETL_GZIP_OUTPUT": str(int(compress))}
            result = self.run_etl(directory, **mode)
            self.assertEqual(result.returncode, 0, result.stderr)
            curated = directory / "curated"
            target = curated.readlink()
            before = {path.name: path.read_bytes() for path in curated.iterdir()}
            for name, old, new, diagnostic in cases:
                with self.subTest(gzip=compress, input=name, diagnostic=diagnostic):
                    path = directory / (name + ".gz" if compress else name)
                    original = path.read_bytes()
                    data = gzip.decompress(original) if compress else original
                    self.assertIn(old, data)
                    changed = data.replace(old, new, 1)
                    path.write_bytes(gzip.compress(changed) if compress else changed)
                    result = self.run_etl(directory, DUMP_TS="2025-02-04T04:05:06Z", **mode)
                    path.write_bytes(original)
                    self.assertEqual(result.returncode, 1, result.stdout)
                    self.assertIn(diagnostic, result.stderr)
                    self.assertEqual(curated.readlink(), target)
                    self.assertEqual({path.name: path.read_bytes() for path in curated.iterdir()}, before)
                    self.assertEqual(list(directory.glob(".curated-*")), [directory / target])


class ArtistRelationProjectionTest(ProjectionTestCase):
    edge_tail = "\t0\t2025-01-01 00:00:00+00\t0\t\t\n"

    def setUp(self):
        super().setUp()
        self.targets = {"recording": (50, 51), "release": (30, 31), "work": (80, 81)}
        self.expected = {}
        for index, (target, (first, second)) in enumerate(self.targets.items()):
            link = 700 + 3 * index
            with (self.raw / "link").open("a") as links, (self.raw / "link_type").open("a") as types:
                for offset, name in enumerate(("credit", r"\143redit", "arrangement")):
                    key = link + offset
                    types.write(f"{key}\t\\N\t0\t00000000-0000-4000-8000-{key:012d}\tartist\t{target}\t{name}"
                                "\tTest\tTest\tTest\tTest\t2025-01-01 00:00:00+00\tf\tt\t0\t0\n")
                    links.write(f"{key}\t{key}" + "\t\\N" * 6 + "\t0\t2025-01-01 00:00:00+00\tf\n")
            edges = ((link, 20, first), (link + 1, 10, first), (link, 10, first),
                     (link + 2, 10, first), (link, 10, second))
            (self.raw / f"l_artist_{target}").write_text("".join(
                f"{key}\t{link_id}\t{artist}\t{entity}{self.edge_tail}"
                for key, (link_id, artist, entity) in enumerate(edges, 1)
            ))
            name = f"mbdump-artist_{target}.tsv"
            self.expected[target] = (ROOT / "tests" / "expected" / name).read_bytes()
        # A sidecar-only link must not admit an otherwise orphaned URL edge.
        for table, entity in (("l_artist_url", 10), ("l_recording_url", 50)):
            with (self.raw / table).open("a") as rows:
                rows.write(f"800\t700\t{entity}\t999{self.edge_tail}")

    def pack(self):
        packed = self.tmp / "packed"
        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        extracted = subprocess.run([sys.executable, str(EXTRACT), str(packed)], input=tar_bytes(members),
                                   capture_output=True, check=False)
        self.assertEqual(extracted.returncode, 0, extracted.stderr)
        return packed

    def check_outputs(self, expected):
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        for table in ("link", "link_type", *(f"l_artist_{target}" for target in self.targets)):
            path = self.raw / table
            path.write_bytes(b"".join(reversed(path.read_bytes().splitlines(keepends=True))))
        packed = self.pack()
        result = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        for target, contents in expected.items():
            name = f"mbdump-artist_{target}.tsv"
            self.assertEqual((self.curated / name).read_bytes(), contents)
            self.assertEqual(gzip.decompress((packed / "curated" / (name + ".gz")).read_bytes()), contents)
            self.assertFalse((packed / f"l_artist_{target}.gz").exists())
        for legacy in ("artist_url", "recording_url", "recording_music_video"):
            name = f"mbdump-{legacy}.tsv"
            self.assertEqual((self.curated / name).read_bytes(), (ROOT / "tests" / "expected" / name).read_bytes())

    def test_artist_relationships_are_deduplicated_and_match_streamed_gzip(self):
        self.check_outputs(self.expected)

    def test_empty_artist_relationships_emit_empty_sidecars(self):
        for target in self.targets:
            (self.raw / f"l_artist_{target}").write_bytes(b"")
        self.check_outputs(dict.fromkeys(self.targets, b""))

    def pack_published(self):
        packed = self.pack()
        result = self.run_etl(packed, MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        return packed

    def assert_refusal(self, raw, *diagnostics, gzip_output="1", consume="1"):
        current = raw / "curated"
        snapshot = current.readlink()
        before = {p.name: p.read_bytes() for p in current.iterdir()}
        mode = {"MB_ETL_GZIP_OUTPUT": gzip_output, "MB_ETL_CONSUME_INPUTS": consume}
        refused = self.run_etl(raw, DUMP_TS="2025-02-04T04:05:06Z", **mode)
        self.assertEqual(refused.returncode, 1, refused.stdout)
        for diagnostic in diagnostics:
            self.assertIn(diagnostic, refused.stderr)
        self.assertEqual(current.readlink(), snapshot)
        self.assertEqual({p.name: p.read_bytes() for p in current.iterdir()}, before)
        self.assertEqual(list(raw.glob(".curated-*")), [raw / snapshot])

    def test_bad_artist_relationship_inputs_refuse_before_consumption(self):
        packed = self.pack_published()
        plain, compressed = packed / "l_artist_work", packed / "l_artist_work.gz"
        plain.touch()
        for diagnostic, remove in (
            (f"ambiguous input: both {plain} and {compressed} exist", ()),
            (f"missing input file: {plain}", (plain, compressed)),
        ):
            with self.subTest(diagnostic=diagnostic):
                for path in remove:
                    path.unlink()
                inputs = {p.name: p.read_bytes() for p in packed.iterdir() if p.is_file()}
                self.assert_refusal(packed, diagnostic)
                self.assertEqual({p.name: p.read_bytes() for p in packed.iterdir() if p.is_file()}, inputs)

    def test_consuming_gzip_refuses_dangling_artist_relationship(self):
        packed = self.pack_published()
        path = packed / "l_artist_work.gz"
        payload = gzip.decompress(path.read_bytes())
        path.write_bytes(gzip.compress(payload.replace(b"\t20\t80\t", b"\t20\t999\t", 1)))
        self.assert_refusal(packed, "l_artist_work: references missing work 999")
        self.assertFalse(path.exists())

    def test_refused_artist_references_preserve_published_snapshot(self):
        packed = self.pack()
        for raw, suffix, mode in ((self.raw, "", "0"), (packed, ".gz", "1")):
            published = self.run_etl(raw, MB_ETL_GZIP_OUTPUT=mode)
            self.assertEqual(published.returncode, 0, published.stderr)
            for index, (target, (first, *_)) in enumerate(self.targets.items()):
                key = 700 + 3 * index
                for table, old, new, error in (
                    (f"l_artist_{target}", f"\t20\t{first}\t", "\t20\t999\t", f"missing {target} 999"),
                    (f"l_artist_{target}", f"\t20\t{first}\t", f"\t999\t{first}\t", "missing artist 999"),
                    (f"l_artist_{target}", f"\t{key}\t20\t", "\t999\t20\t", "missing link 999"),
                    ("link", f"\n{key}\t{key}\t", f"\n{key}\t999\t", "missing link_type 999"),
                    ("link", f"\n{key}\t{key}\t", f"\n{key}\t506\t", f"is not artist-to-{target}"),
                ):
                    with self.subTest(mode=mode, target=target, error=error):
                        path = raw / (table + suffix)
                        original = path.read_bytes()
                        payload = gzip.decompress(original) if suffix else original
                        self.assertIn(old.encode(), payload)
                        changed = payload.replace(old.encode(), new.encode(), 1)
                        path.write_bytes(gzip.compress(changed) if suffix else changed)
                        try:
                            self.assert_refusal(raw, f"l_artist_{target}: ", error, gzip_output=mode, consume="0")
                        finally:
                            path.write_bytes(original)


class LabelAliasProjectionTest(AliasProjectionTestCase):
    entity = "label"

    def test_decoded_keys_and_raw_winners_match_streamed_gzip(self):
        self.assert_plain_and_streamed_output((ROOT / "tests" / "expected" / self.output).read_bytes())
        alias_mbids = self.curated_mbids("label_alias")
        self.assertLess(alias_mbids, self.curated_mbids("label"))
        self.assertTrue(alias_mbids.isdisjoint(self.curated_mbids("release")))

    def test_empty_label_alias_table_emits_empty_sidecar(self):
        (self.raw / "label_alias").write_bytes(b"")
        self.assert_plain_and_streamed_output(b"")

    def test_label_alias_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        self.assertIn("label_alias=4,", published.stdout)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        path = self.raw / "label_alias"
        aliases = path.read_bytes()
        first_row = aliases.splitlines(keepends=True)[0]
        for changed, error in (
            (None, "missing input file:"),
            (b"broken\n", "expected 16 fields"),
            (aliases + first_row, "UNIQUE constraint failed: label_alias.id"),
            # 30 exists as a release, not as a label. Other aliases stay valid.
            (aliases.replace(b"930\t71\t", b"930\t30\t", 1), "label_alias references missing label 30"),
            (aliases.replace(b"\tt\tf\n", b"\ttrue\tf\n", 1), "invalid primary_for_locale"),
            (aliases.replace(rb"\303\x89tiquette", rb"\303tiquette", 1), "invalid COPY text in name or locale"),
            (aliases.replace(rb"Quartz\tRecords", rb"Quartz\377", 1), "invalid COPY text in sort_name"),
        ):
            with self.subTest(error=error):
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-03T04:05:07Z")
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


class LabelProjectionTest(ProjectionTestCase):
    def test_labels_and_decoded_edges_match_streamed_gzip(self):
        # 255 decoded characters, but more than 255 COPY bytes.
        boundary = "é" * 254 + r"\t"
        types = self.raw / "label_type"
        types.write_bytes(types.read_bytes().replace(rb"Original\tProduction", boundary.encode()))
        edges = self.raw / "release_label"
        edges.write_bytes(edges.read_bytes() + f"11\t30\t71\t{boundary}\t\\N\n".encode())
        plain = self.run_etl(self.raw)
        self.assertEqual(plain.returncode, 0, plain.stderr)
        for name in ("mbdump-label.tsv", "mbdump-release_label.tsv"):
            self.assertEqual((self.curated / name).read_bytes(), (ROOT / "tests" / "expected" / name).read_bytes())

        members = {f"mbdump/{p.name}": p.read_bytes() for p in self.raw.iterdir() if p.is_file()}
        tables = ("label", "label_type", "release_label")
        for table in tables:
            key = f"mbdump/{table}"
            members[key] = b"".join(reversed(members[key].splitlines(keepends=True)))
        # The streamed copy has only the alternate spelling for this edge.
        key = "mbdump/release_label"
        members[key] = b"".join(line for line in members[key].splitlines(keepends=True)
                                if not line.startswith((b"5\t", b"7\t")))
        packed = self.tmp / "packed"
        extract = subprocess.run([sys.executable, str(EXTRACT), str(packed)],
                                 input=tar_bytes(members), capture_output=True, check=False)
        self.assertEqual(extract.returncode, 0, extract.stderr)
        for table in tables:
            self.assertTrue((packed / f"{table}.gz").is_file())
        gzip_etl = self.run_etl(packed, MB_ETL_CONSUME_INPUTS="1", MB_ETL_GZIP_OUTPUT="1")
        self.assertEqual(gzip_etl.returncode, 0, gzip_etl.stderr)
        for table in tables:
            self.assertFalse((packed / f"{table}.gz").exists())
        self.assertEqual(
            {p.name.removesuffix(".gz"): gzip.decompress(p.read_bytes()) for p in (packed / "curated").iterdir()},
            {p.name: p.read_bytes() for p in self.curated.iterdir()},
        )

    def test_only_null_labels_produce_no_edges(self):
        (self.raw / "release_label").write_bytes(b"4\t30\t\\N\tNot an edge\t\\N\n")
        result = self.run_etl(self.raw)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.curated / "mbdump-release_label.tsv").read_bytes(), b"")

    def test_label_refusals_preserve_published_snapshot(self):
        published = self.run_etl(self.raw)
        self.assertEqual(published.returncode, 0, published.stderr)
        target = self.curated.readlink()
        before = {p.name: p.read_bytes() for p in self.curated.iterdir()}
        labels = (self.raw / "label").read_bytes()
        types = (self.raw / "label_type").read_bytes()
        edges = (self.raw / "release_label").read_bytes()
        releases = (self.raw / "release").read_bytes()
        too_long = "é".encode() * 255 + rb"\t"
        for table, changed, error in (
            ("label_type", None, "missing input file:"),
            ("label", b"broken\n", "expected 16 fields"),
            ("release_label", b"broken\n", "expected 5 fields"),
            ("label", labels.replace(b"\t12345\t1\t", b"\t12345\t999\t"), "references missing label_type 999"),
            ("label", labels.replace(b"\t12345\t", b"\t2147483648\t"), "invalid label code"),
            ("label", labels.replace(b"72\t", b"\\N\t", 1), "invalid id"),
            ("label", labels.replace(b"62626262-6262-4626-8626-626262626262", b"bad"), "invalid label MBID"),
            ("label", labels.replace(b"Untyped Label", rb"\N"), "name must not be NULL"),
            ("label_type", types.replace(rb"Original\tProduction", rb"\000"), "invalid COPY text in name"),
            ("label_type", types.replace(rb"Original\tProduction", too_long), "name exceeds 255 characters"),
            ("release_label", edges + b"1\t31\t71\tDifferent number\t\\N\n", "UNIQUE constraint failed: release_label.id"),
            ("release_label", edges + b"11\t30\t71\t" + too_long + b"\t\\N\n", "catalogue number exceeds 255 characters"),
            ("release_label", edges + b"11\t999\t\\N\t\\N\t\\N\n", "references missing release 999"),
            ("release_label", edges + b"11\t30\t999\t\\N\t\\N\n", "references missing label 999"),
            ("release_label", edges + b"11\t\\N\t71\t\\N\t\\N\n", "invalid release"),
            ("release_label", edges + b"11\t30\t+71\t\\N\t\\N\n", "invalid label"),
            ("release_label", edges + b"11\t30\t71\t\\377\t\\N\n", "invalid COPY text in catalogue number"),
            ("release", releases.replace(b"55555555-5555-4555-8555-555555555555", b"bad"), "invalid release MBID"),
        ):
            with self.subTest(table=table, error=error):
                path = self.raw / table
                original = path.read_bytes()
                if changed is None:
                    path.unlink()
                else:
                    path.write_bytes(changed)
                refused = self.run_etl(self.raw, DUMP_TS="2025-02-04T04:05:06Z")
                path.write_bytes(original)
                self.assertEqual(refused.returncode, 1, refused.stdout)
                self.assertIn(error, refused.stderr)
                self.assertEqual(self.curated.readlink(), target)
                self.assertEqual({p.name: p.read_bytes() for p in self.curated.iterdir()}, before)
                self.assertEqual(list(self.raw.glob(".curated-*")), [self.raw / target])


if __name__ == "__main__":
    unittest.main()
