#!/usr/bin/env python3
"""Test executable input checks without privileges or an executing candidate."""
import hashlib
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_custody_inputs as custody


class ExecutableCustody(unittest.TestCase):
    def setUp(self):
        self.root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.bin = self.root / "bin"
        self.bin.mkdir(mode=0o755)
        self.tool = self.bin / "tool"
        self.tool.write_bytes(b"synthetic executable bytes")
        self.tool.chmod(0o755)
        self.digest = hashlib.sha256(self.tool.read_bytes()).hexdigest()
        self.root_fd = os.open(self.root, os.O_DIRECTORY)
        self.addCleanup(os.close, self.root_fd)

    def verify(self, expected=None):
        # Exercise the same descriptor walk under a synthetic supervisor UID.
        return custody._verify_at(self.root_fd, ["bin", "tool"], os.getuid(), expected or self.digest)

    def test_returns_verified_read_only_descriptor(self):
        fd = self.verify()
        self.addCleanup(os.close, fd)
        self.assertFalse(os.get_inheritable(fd))
        self.assertEqual(os.read(fd, 100), b"synthetic executable bytes")
        with self.assertRaises(OSError):
            os.write(fd, b"different bytes")

    def test_rejects_untrusted_owner_and_writable_path_components(self):
        with self.assertRaisesRegex(ValueError, "supervisor-owned"):
            custody._verify_at(self.root_fd, ["bin", "tool"], os.getuid() + 1, self.digest)
        for node in (self.root, self.bin, self.tool):
            with self.subTest(node=node.name):
                mode = node.stat().st_mode & 0o777
                node.chmod(mode | 0o020)
                try:
                    with self.assertRaisesRegex(ValueError, "write-protected"):
                        self.verify()
                finally:
                    node.chmod(mode)

    def test_rejects_symlink_ancestors_and_executable_aliases(self):
        real_bin = self.root / "real-bin"
        self.bin.rename(real_bin)
        self.bin.symlink_to(real_bin, target_is_directory=True)
        with self.assertRaises(OSError):
            self.verify()
        self.bin.unlink()
        real_bin.rename(self.bin)
        real_tool = self.bin / "real-tool"
        self.tool.rename(real_tool)
        self.tool.symlink_to(real_tool)
        with self.assertRaises(ValueError):
            self.verify()
        self.tool.unlink()
        os.link(real_tool, self.tool)
        with self.assertRaisesRegex(ValueError, "no hard links"):
            self.verify()

    def test_rejects_nonregular_nonexecutable_and_privileged_files(self):
        for mode in (0o644, 0o4755, 0o2755):
            with self.subTest(mode=oct(mode)):
                self.tool.chmod(mode)
                with self.assertRaises(ValueError):
                    self.verify()
        self.tool.unlink()
        os.mkfifo(self.tool, 0o755)
        with self.assertRaises(ValueError):
            self.verify()
        self.tool.unlink()
        self.tool.mkdir()
        with self.assertRaises(ValueError):
            self.verify()

    def test_rejects_capabilities_and_digest_mismatch(self):
        with patch.object(custody.os, "getxattr", return_value=b"capability record"):
            with self.assertRaisesRegex(ValueError, "file capabilities"):
                self.verify()
        with self.assertRaisesRegex(ValueError, "digest does not match"):
            self.verify("0" * 64)

    def test_rejects_noncanonical_paths_and_malformed_digests(self):
        for path in ("", "/", "bin/tool", "//bin/tool", "/bin//tool", "/bin/./tool", "/bin/../tool"):
            with self.subTest(path=path), self.assertRaisesRegex(ValueError, "absolute executable path"):
                custody.open_verified_executable(path, self.digest)
        with self.assertRaisesRegex(ValueError, "SHA-256 digest"):
            custody.open_verified_executable("/bin/tool", "invalid")


if __name__ == "__main__":
    unittest.main()
