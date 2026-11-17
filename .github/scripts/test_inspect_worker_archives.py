"""Synthetic checks for the non-extracting Worker archive diagnostic."""

import hashlib
import importlib.util
import io
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("inspector", HERE / "inspect_worker_archives.py")
inspector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inspector)


def archive(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as tar:
        for name, content, kind in entries:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.size = len(content) if kind == tarfile.REGTYPE else 0
            tar.addfile(member, io.BytesIO(content) if member.size else None)
    return output.getvalue()


def inspect(entries):
    data = archive(entries)
    with mock.patch.dict(inspector.CHECKSUMS, {"test": hashlib.sha256(data).hexdigest()}):
        return inspector.inspect_archive(data, "test")


class ArchiveInspectionTests(unittest.TestCase):
    def test_reports_references_without_extracting(self):
        matches = inspect([("worker-test/src/date.rs", b"use chrono::Utc;\n", tarfile.REGTYPE)])
        self.assertEqual(matches, [{"version": "test", "file": "src/date.rs", "line": 1, "text": "use chrono::Utc;"}])

    def test_rejects_wrong_checksum_and_size(self):
        with self.assertRaises(ValueError):
            inspector.inspect_archive(b"not an archive", "0.8.7")
        with mock.patch.object(inspector, "MAX_ARCHIVE", 1), self.assertRaises(ValueError):
            inspector.inspect_archive(b"xx", "0.8.7")

    def test_rejects_paths_duplicates_and_nonfiles(self):
        good = ("worker-test/src/date.rs", b"chrono", tarfile.REGTYPE)
        for entries in ([good, good], [("worker-test/../escape", b"chrono", tarfile.REGTYPE)],
                        [("other/src/date.rs", b"chrono", tarfile.REGTYPE)],
                        [("worker-test/src/link", b"", tarfile.SYMTYPE)]):
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                inspect(entries)

    def test_rejects_missing_matches_and_output_limits(self):
        with self.assertRaises(ValueError):
            inspect([("worker-test/src/date.rs", b"nothing", tarfile.REGTYPE)])
        with self.assertRaises(ValueError):
            inspect([("worker-test/src/date.rs", b"chrono" * 200, tarfile.REGTYPE)])
        with mock.patch.object(inspector, "MAX_TOTAL", 2), self.assertRaises(ValueError):
            inspect([("worker-test/src/date.rs", b"chrono", tarfile.REGTYPE)])

    def test_lock_identity_and_duplicates(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "Cargo.lock"
            checksum = inspector.CHECKSUMS["0.8.7"]
            entry = ('[[package]]\nname = "worker"\nversion = "0.8.7"\n'
                     'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
                     f'checksum = "{checksum}"\n')
            with mock.patch.object(inspector, "LOCKS", {"Cargo.lock": "0.8.7"}):
                path.write_text(entry)
                inspector.verify_locks(root)
                for invalid in (entry + entry, entry.replace(checksum, "0" * 64), entry.replace("0.8.7", "0.9.0")):
                    path.write_text(invalid)
                    with self.assertRaises(ValueError):
                        inspector.verify_locks(root)

    def test_redirect_and_runner_guards(self):
        with self.assertRaises(ValueError):
            inspector.NoRedirect().redirect_request(None, None, 302, "", {}, "https://example.invalid")
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), self.assertRaises(RuntimeError):
            inspector.main()
        result = mock.Mock(stdout="actual\n")
        with mock.patch.dict(os.environ, {"GITHUB_ACTIONS": "true", "EXPECTED_HEAD_SHA": "other"}), \
             mock.patch.object(inspector.subprocess, "run", return_value=result), self.assertRaises(RuntimeError):
            inspector.main()

    def test_job_admission_and_read_only_scope(self):
        text = (HERE.parent / "workflows/ci.yml").read_text()
        section = text.split("  worker-sdk-inspection:\n", 1)[1].split("\n  public-pr-security:", 1)[0]
        for required in ("github.event_name == 'pull_request'", "github.event.pull_request.draft == true",
                         "github.event.pull_request.head.repo.full_name == github.repository",
                         "github.event.pull_request.head.ref == 'chore/worker-sdk-clock-regressions'",
                         'test "$state" = open && test "$draft" = true',
                         'test "$repository" = "$REPOSITORY" && test "$branch" = chore/worker-sdk-clock-regressions',
                         'test "$sha" = "$EXPECTED_SHA"', "persist-credentials: false",
                         "ref: ${{ github.event.pull_request.head.sha }}",
                         "EXPECTED_HEAD_SHA: ${{ github.event.pull_request.head.sha }}",
                         "python3 -I .github/scripts/test_inspect_worker_archives.py",
                         "python3 -I .github/scripts/inspect_worker_archives.py", "timeout-minutes: 20"):
            self.assertIn(required, section)
        for forbidden in ("write", "secrets.", "upload-artifact"):
            self.assertNotIn(forbidden, section)


if __name__ == "__main__":
    unittest.main()
