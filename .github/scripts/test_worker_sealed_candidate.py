#!/usr/bin/env python3
"""Focused tests for the experimental immutable consumer input."""
import hashlib
import os
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_sealed_candidate as sealed


class SealedCandidate(unittest.TestCase):
    def test_consumer_receives_the_verified_immutable_snapshot(self):
        candidate = b"candidate bytes"
        seen = []

        def verify(snapshot):
            seen.append(snapshot)

        def run(command, **kwargs):
            self.assertTrue(kwargs["check"])
            fd = kwargs["pass_fds"][0]
            self.assertEqual(command[-1], f"/proc/self/fd/{fd}")
            self.assertEqual(Path(command[-1]).read_bytes(), candidate)
            with self.assertRaises(OSError):
                os.pwrite(fd, b"x", 0)
            with self.assertRaises(OSError):
                os.ftruncate(fd, 0)

        digest = sealed.consume_sealed(candidate, verify, ["/bin/true"], cwd="/", env={}, run=run)
        self.assertEqual(seen, [candidate])
        self.assertEqual(digest, hashlib.sha256(candidate).hexdigest())

    def test_real_child_reads_snapshot_and_cannot_modify_it(self):
        child = """\
import os, sys
path = sys.argv[1]
assert open(path, 'rb').read() == b'candidate bytes'
fd = os.open(path, os.O_RDWR)
try:
    try:
        os.pwrite(fd, b'x', 0)
    except PermissionError:
        pass
    else:
        raise AssertionError('sealed child descriptor was writable')
    try:
        os.ftruncate(fd, 0)
    except PermissionError:
        pass
    else:
        raise AssertionError('sealed child descriptor was truncatable')
finally:
    os.close(fd)
"""
        digest = sealed.consume_sealed(
            b"candidate bytes", lambda _: None, [sys.executable, "-I", "-c", child], cwd="/", env={}
        )
        self.assertEqual(digest, hashlib.sha256(b"candidate bytes").hexdigest())

    def test_rejection_never_runs_consumer(self):
        def reject(_):
            raise ValueError("inspection rejected")

        with patch.object(sealed.subprocess, "run") as run:
            with self.assertRaisesRegex(ValueError, "inspection rejected"):
                sealed.consume_sealed(b"x", reject, ["/bin/true"], cwd="/", env={}, run=run)
            run.assert_not_called()
        for candidate in (b"", bytearray(b"x"), b"x" * (sealed.MAX_CANDIDATE_BYTES + 1)):
            with self.subTest(size=len(candidate)), self.assertRaises(ValueError):
                sealed.consume_sealed(candidate, lambda _: None, ["/bin/true"], cwd="/", env={})
        with self.assertRaises(ValueError):
            sealed.consume_sealed(b"x", lambda _: None, ["true"], cwd="/", env={})

    def test_consumer_failure_propagates(self):
        def fail(*args, **kwargs):
            raise subprocess.CalledProcessError(1, args[0])

        with self.assertRaises(subprocess.CalledProcessError):
            sealed.consume_sealed(b"x", lambda _: None, ["/bin/false"], cwd="/", env={}, run=fail)


if __name__ == "__main__":
    unittest.main()
