#!/usr/bin/env python3
"""Focused tests for the experimental immutable consumer input."""
import hashlib
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import call, patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_sealed_candidate as sealed


class SealedCandidate(unittest.TestCase):
    def test_build_experiment_passes_fresh_output_to_verifier_and_consumer(self):
        seen = []
        outputs = []

        def build(output):
            self.assertFalse(output.exists())
            outputs.append(output)
            output.write_bytes(b"built candidate")

        def verify(data):
            seen.append(("verify", data))
            outputs[0].write_bytes(b"replaced after snapshot")

        def run(argv, **kwargs):
            seen.append(("consume", Path(argv[-1]).read_bytes()))

        digest = sealed.experiment_build_and_consume(build, verify, ["/bin/true"], cwd="/", env={}, run=run)
        self.assertEqual(seen, [("verify", b"built candidate"), ("consume", b"built candidate")])
        self.assertEqual(digest, hashlib.sha256(b"built candidate").hexdigest())
        self.assertFalse(outputs[0].parent.exists())

    def test_build_experiment_stops_on_build_output_or_verifier_failure(self):
        def fail_build(output):
            output.write_bytes(b"partial")
            raise subprocess.CalledProcessError(1, "build")

        def return_status(output):
            output.write_bytes(b"partial")
            return 1

        def missing_output(output):
            pass

        def invalid_output(output):
            output.write_bytes(b"")

        def valid_output(output):
            output.write_bytes(b"x")

        def reject(_):
            raise ValueError("rejected")

        for build, verify, error, message in (
            (fail_build, lambda _: None, subprocess.CalledProcessError, "non-zero exit status 1"),
            (return_status, lambda _: None, ValueError, "build callback must return None"),
            (missing_output, lambda _: None, FileNotFoundError, "candidate.wasm"),
            (invalid_output, lambda _: None, ValueError, "invalid candidate output file"),
            (valid_output, reject, ValueError, "^rejected$"),
        ):
            with self.subTest(build=build), patch.object(sealed.subprocess, "run") as run:
                with self.assertRaisesRegex(error, message):
                    sealed.experiment_build_and_consume(build, verify, ["/bin/true"], cwd="/", env={}, run=run)
                run.assert_not_called()

    def test_snapshot_build_output_is_bounded_and_rejects_nonfiles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "candidate.wasm"
            output.write_bytes(b"candidate bytes")
            snapshot = sealed.snapshot_build_output(output)
            output.write_bytes(b"replaced")
            self.assertEqual(snapshot, b"candidate bytes")
            link = root / "link"
            link.symlink_to(output)
            with self.assertRaises(OSError):
                sealed.snapshot_build_output(link)
            with self.assertRaises(ValueError):
                sealed.snapshot_build_output(root)
            output.write_bytes(b"")
            with self.assertRaises(ValueError):
                sealed.snapshot_build_output(output)
            with output.open("wb") as f:
                f.truncate(sealed.MAX_CANDIDATE_BYTES + 1)
            with self.assertRaises(ValueError):
                sealed.snapshot_build_output(output)

    def test_snapshot_read_is_bounded_when_output_grows(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "candidate.wasm"
            output.write_bytes(b"wasm")
            with patch.object(sealed, "MAX_CANDIDATE_BYTES", 4):
                self.assertEqual(sealed.snapshot_build_output(output), b"wasm")
            with patch.object(sealed, "MAX_CANDIDATE_BYTES", 8), patch.object(sealed.os, "fdopen") as fdopen:
                reader = fdopen.return_value.__enter__.return_value
                reader.read.side_effect = [b"wasm", b"!"]
                with self.assertRaisesRegex(ValueError, "candidate output size changed while reading"):
                    sealed.snapshot_build_output(output)
                self.assertEqual(fdopen.call_args.kwargs["buffering"], 0)
                self.assertEqual(reader.read.call_args_list, [call(4), call(1)])

    def test_snapshot_rejects_short_read(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "candidate.wasm"
            output.write_bytes(b"wasm")
            with patch.object(sealed.os, "fdopen") as fdopen:
                reader = fdopen.return_value.__enter__.return_value
                reader.read.side_effect = [b"was", b""]
                with self.assertRaisesRegex(ValueError, "candidate output size changed while reading"):
                    sealed.snapshot_build_output(output)
                reader.read.assert_called_once_with(4)

    def test_snapshot_is_the_sealed_consumer_input(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "candidate.wasm"
            output.write_bytes(b"built candidate")
            snapshot = sealed.snapshot_build_output(output)
            seen = []

            def verify(data):
                seen.append(data)
                output.write_bytes(b"different candidate")

            def run(argv, **kwargs):
                self.assertEqual(Path(argv[-1]).read_bytes(), b"built candidate")

            sealed.consume_sealed(snapshot, verify, ["/bin/true"], cwd="/", env={}, run=run)
            self.assertEqual(seen, [b"built candidate"])

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

    def test_bytes_subclass_cannot_bypass_validation(self):
        class MisleadingBytes(bytes):
            def __len__(self):
                return 1

            def __eq__(self, other):
                return True

        candidate = MisleadingBytes(b"candidate bytes")
        with patch.object(sealed.os, "memfd_create") as create:
            with self.assertRaisesRegex(ValueError, "invalid candidate bytes"):
                sealed.consume_sealed(candidate, lambda _: None, ["/bin/true"], cwd="/", env={})
            create.assert_not_called()

    def test_consumer_command_is_copied_before_verification(self):
        command = ["/bin/true", "original"]
        launched = []

        def verify(_):
            command[:] = ["relative-executable", "changed"]

        def run(argv, **kwargs):
            launched.append(argv)

        sealed.consume_sealed(b"x", verify, command, cwd="/", env={}, run=run)
        self.assertEqual(launched[0][:-1], ["/bin/true", "original"])

    def test_consumer_failure_propagates(self):
        def fail(*args, **kwargs):
            raise subprocess.CalledProcessError(1, args[0])

        with self.assertRaises(subprocess.CalledProcessError):
            sealed.consume_sealed(b"x", lambda _: None, ["/bin/false"], cwd="/", env={}, run=fail)


if __name__ == "__main__":
    unittest.main()
