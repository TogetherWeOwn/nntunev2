"""CLI regressions for offline extract admission."""

import bz2
import gzip
import os
import subprocess
import sys
from unittest import skipUnless

from test_extract import EXTRACT, ExtractCase, dumps, item_entity, rows, stmt


class TestExtractBoundaries(ExtractCase):
    def test_bzip2_requires_complete_members(self):
        packed = bz2.compress(b"[\n") + bz2.compress(b"]\n") + bz2.compress(b"")
        for tail in (b"trailing data", b"BZh", bz2.compress(b"")[:-1]):
            self.fails(self.write_input(packed + tail, "corrupt.json.bz2"))
        self.fails(self.write_input(b"", "empty.jsonl.bz2"), "--format", "jsonl")
        self.ok(self.write_input(packed, "valid.json.bz2"))
        self.ok(self.write_input(bz2.compress(b""), "valid.jsonl.bz2"), "--format", "jsonl")

    def test_empty_gzip_file_has_no_valid_envelope(self):
        self.fails(self.write_input(b"", "empty.jsonl.gz"), "--format", "jsonl")
        self.ok(self.write_input(gzip.compress(b""), "valid.jsonl.gz"), "--format", "jsonl")

    def test_nil_mbid_is_not_projected(self):
        nil = "00000000-0000-0000-0000-000000000000"
        path = self.write_input(dumps([item_entity("Q1", P434=[stmt("P434", nil)],
                                                 P136=[stmt("P136", "Q2")])]))
        self.assertEqual(rows(self.ok(path), "artist_genres.jsonl"), [])

    @skipUnless(hasattr(os, "mkfifo"), "requires a POSIX FIFO")
    def test_fifo_is_refused_without_a_writer(self):
        path = os.path.join(self.tmp, "input.json")
        os.mkfifo(path)
        self.fails(path)

def test_regular_file_without_nonblock(tmp_path):
    source, out = tmp_path / "input.json", tmp_path / "output"
    source.write_bytes(b"[]\n")
    run = ("import os, runpy, sys\nfrom types import SimpleNamespace\n"
           "sys.argv.pop(0)\nmain = runpy.run_path(sys.argv[0])['main']\n"
           "main.__globals__['os'] = SimpleNamespace(open=os.open, fstat=os.fstat)\n"
           "raise SystemExit(main())")
    subprocess.run([sys.executable, "-I", "-c", run, EXTRACT, "--input", str(source),
                    "--output", str(out), "--dump-version", "synthetic-v1"], check=True, timeout=15)
    assert (out / "manifest.json").is_file()
