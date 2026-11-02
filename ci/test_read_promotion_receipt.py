"""Unit tests for read_promotion_receipt.

Run with `python3 ci/test_read_promotion_receipt.py` (stdlib only, no
install) and wired into the ci-ok-required `release-proof` job, so
loosening an archive bound goes red. All archives below are built in
memory; nothing touches the network or the filesystem.
"""

import importlib.util
import io
import os
import subprocess
import sys
import unittest
import zipfile

CI_DIR = os.path.dirname(os.path.abspath(__file__))
HELPER = os.path.join(CI_DIR, "read-promotion-receipt.py")

_spec = importlib.util.spec_from_file_location(
    "read_promotion_receipt", HELPER
)
_module = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_module)

read_receipt = _module.read_receipt
MAX_ARCHIVE = _module.MAX_ARCHIVE
MAX_RECEIPT = _module.MAX_RECEIPT

RECEIPT = b'{"schema":1,"repository":"TogetherWeOwn/nntunev2"}'


def make_archive(members):
    """Build a zip archive in memory from (name, bytes) pairs."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as archive:
        for name, data in members:
            archive.writestr(name, data)
    return buf.getvalue()


class TestReadReceipt(unittest.TestCase):
    def test_accepts_single_member_receipt(self):
        data = make_archive([("promotion.json", RECEIPT)])
        self.assertEqual(read_receipt(data), RECEIPT)

    def test_refuses_empty_input(self):
        with self.assertRaises(ValueError):
            read_receipt(b"")

    def test_refuses_garbage_input(self):
        # zipfile.BadZipFile is not a ValueError; the CLI catches it
        # alongside ValueError, so both spellings refuse here.
        with self.assertRaises((ValueError, zipfile.BadZipFile)):
            read_receipt(b"this is not a zip archive at all")

    def test_refuses_oversized_archive(self):
        with self.assertRaises(ValueError):
            read_receipt(b"x" * (MAX_ARCHIVE + 1))

    def test_refuses_multi_member_archive(self):
        data = make_archive(
            [("promotion.json", RECEIPT), ("promotion.json", RECEIPT)]
        )
        # zipfile keeps duplicate names as two members.
        self.assertGreater(len(data), 0)
        with self.assertRaises(ValueError):
            read_receipt(data)

    def test_refuses_wrong_member_name(self):
        with self.assertRaises(ValueError):
            read_receipt(make_archive([("receipt.json", RECEIPT)]))

    def test_refuses_oversized_member(self):
        with self.assertRaises(ValueError):
            read_receipt(make_archive([("promotion.json", b"{}".ljust(MAX_RECEIPT + 1))]))

    def test_refuses_non_utf8_member(self):
        with self.assertRaises(ValueError):
            read_receipt(make_archive([("promotion.json", b"\xff\xfe\x00bad")]))

    def test_cli_refuses_empty_stdin(self):
        proc = subprocess.run(
            [sys.executable, HELPER],
            input=b"",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
        )
        self.assertNotEqual(proc.returncode, 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
