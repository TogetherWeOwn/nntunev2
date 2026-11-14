"""Synthetic tests for offline extract provenance receipts."""

import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

MIRROR = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(MIRROR))
from extract_receipts import FILES, MAX_ROWS, verify_receipts

EXTRACT = MIRROR.parent / "genre-extract" / "extract.py"
FIXTURE = MIRROR.parent / "genre-extract" / "tests" / "fixtures" / "synthetic.json"


class ReceiptTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.directory = Path(self.tmp.name) / "extract"
        subprocess.run([sys.executable, "-I", str(EXTRACT), "--input", str(FIXTURE),
                        "--output", str(self.directory), "--dump-version", "synthetic-v1"],
                       check=True, capture_output=True, timeout=120)
        self.path = self.directory / "manifest.json"
        self.manifest = json.loads(self.path.read_bytes())

    def write_manifest(self):
        self.path.write_text(json.dumps(self.manifest), encoding="utf-8")

    def test_extractor_output_and_manifest_digest(self):
        manifest, digest, outputs = verify_receipts(self.directory)
        self.assertEqual(manifest, self.manifest)
        self.assertEqual(digest, hashlib.sha256(self.path.read_bytes()).hexdigest())
        self.assertEqual(set(outputs), set(FILES))
        for name, raw in outputs.items():
            self.assertEqual(raw, (self.directory / name).read_bytes())

    def test_rejects_bad_provenance_and_shapes(self):
        for field, value in (("schema", "unknown"), ("extractor_version", 1),
                             ("extractor_sha256", "x"), ("language", "EN")):
            with self.subTest(field=field):
                original = self.manifest[field]
                self.manifest[field] = value
                self.write_manifest()
                with self.assertRaises(ValueError):
                    verify_receipts(self.directory)
                self.manifest[field] = original
        self.manifest["source"]["license"] = "CC-BY-NC-SA"
        self.write_manifest()
        with self.assertRaises(ValueError):
            verify_receipts(self.directory)

    def test_rejects_duplicate_keys_and_nonfinite_numbers(self):
        for raw in (b'{"schema":1,"schema":2}', b'{"schema":NaN}'):
            self.path.write_bytes(raw)
            with self.assertRaises(ValueError):
                verify_receipts(self.directory)

    def test_rejects_tampering_and_invalid_receipts(self):
        name = "genres.jsonl"
        path = self.directory / name
        original = path.read_bytes()
        for raw in (original + b" ", original.rstrip(b"\n")):
            with self.subTest(raw=raw[-10:]):
                path.write_bytes(raw)
                with self.assertRaises(ValueError):
                    verify_receipts(self.directory)
        path.write_bytes(original.rstrip(b"\n"))
        raw = path.read_bytes()
        self.manifest["outputs"][name] = {"sha256": hashlib.sha256(raw).hexdigest(),
                                          "bytes": len(raw), "rows": raw.count(b"\n")}
        self.write_manifest()
        with self.assertRaises(ValueError):
            verify_receipts(self.directory)
        path.write_bytes(original)
        self.manifest["outputs"][name] = {"sha256": hashlib.sha256(original).hexdigest(),
                                          "bytes": len(original), "rows": original.count(b"\n")}
        for field, value in (("bytes", True), ("rows", MAX_ROWS + 1), ("rows", 0), ("sha256", "0" * 64)):
            with self.subTest(field=field):
                old = self.manifest["outputs"][name][field]
                self.manifest["outputs"][name][field] = value
                self.write_manifest()
                with self.assertRaises(ValueError):
                    verify_receipts(self.directory)
                self.manifest["outputs"][name][field] = old

    def test_rejects_missing_and_symlinked_files(self):
        path = self.directory / FILES[0]
        path.unlink()
        with self.assertRaises(FileNotFoundError):
            verify_receipts(self.directory)
        path.symlink_to(self.directory / FILES[1])
        with self.assertRaises(ValueError):
            verify_receipts(self.directory)


if __name__ == "__main__":
    unittest.main()
