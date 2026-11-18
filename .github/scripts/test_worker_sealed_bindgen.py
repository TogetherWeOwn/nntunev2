#!/usr/bin/env python3
"""Tests for the sealed ledger consumer compatibility probe."""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_worker_sealed_bindgen as probe


class SealedBindgenProbeTests(unittest.TestCase):
    def test_passes_verified_snapshot_to_bindgen_before_running_ledger(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "ledger.wasm"
            candidate.write_bytes(b"\x00asm\x01\x00\x00\x00payload")
            output = Path(directory) / "output"
            events = []

            def consume(data, verify, command, *, cwd, env):
                verify(data)
                with self.assertRaises(ValueError):
                    verify(b"not wasm")
                events.append(("bindgen", command))
                self.assertEqual(cwd, os.getcwd())
                self.assertEqual(env, os.environ.copy())

            def run(command, *, check):
                events.append(("node", command))
                self.assertTrue(check)

            with (mock.patch.object(sys, "argv", ["probe", str(candidate), str(output)]),
                  mock.patch.object(probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]),
                  mock.patch.object(probe.sealed, "consume_sealed", side_effect=consume),
                  mock.patch.object(probe.subprocess, "run", side_effect=run)):
                probe.main()

            self.assertEqual(events[0], ("bindgen", ["/tools/wasm-bindgen", "--target", "nodejs",
                             "--out-dir", str(output), "--out-name", "nntune_api"]))
            self.assertEqual(events[1], ("node", ["/tools/node", "-e",
                             "require(process.argv[1]).run_ledger_tests()",
                             str((output / "nntune_api.js").resolve())]))

    def test_bindgen_failure_prevents_ledger_execution(self):
        with tempfile.TemporaryDirectory() as directory:
            candidate = Path(directory) / "ledger.wasm"
            candidate.write_bytes(b"\x00asm\x01\x00\x00\x00")
            with (mock.patch.object(sys, "argv", ["probe", str(candidate), directory]),
                  mock.patch.object(probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]),
                  mock.patch.object(probe.sealed, "consume_sealed", side_effect=subprocess.CalledProcessError(1, "bindgen")),
                  mock.patch.object(probe.subprocess, "run") as run):
                with self.assertRaises(subprocess.CalledProcessError):
                    probe.main()
                run.assert_not_called()


if __name__ == "__main__":
    unittest.main()
