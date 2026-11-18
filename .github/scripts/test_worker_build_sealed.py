#!/usr/bin/env python3
"""Tests for the inert pinned-build to sealed-consumer experiment."""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_worker_build_sealed as probe


class BuildSealedTests(unittest.TestCase):
    def test_build_output_reaches_sealed_consumer_before_node(self):
        events = []
        candidate = b"\x00asm\x01\x00\x00\x00fixture"

        def build(*, env):
            events.append("build")
            artifact = Path(env["RUNNER_TEMP"]) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(candidate)

        def run(command, **kwargs):
            if "pass_fds" in kwargs:
                events.append("bindgen")
                self.assertEqual(command[:-1], [
                    "/tools/wasm-bindgen", "--target", "nodejs", "--out-dir", str(output_dir),
                    "--out-name", "nntune_api",
                ])
                self.assertEqual(Path(command[-1]).read_bytes(), candidate)
            else:
                events.append("node")
                self.assertEqual(command, [
                    "/tools/node", "-e", "require(process.argv[1]).run_ledger_tests()",
                    str((output_dir / "nntune_api.js").resolve()),
                ])
            self.assertTrue(kwargs["check"])

        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]
        ):
            output_dir = Path(directory).resolve() / "fresh-sealed-ledger-js"
            relative_output_dir = Path(os.path.relpath(output_dir))
            probe.experiment(relative_output_dir, env={"PATH": "/tools"}, build=build, run=run)
        self.assertEqual(events, ["build", "bindgen", "node"])

    def test_build_failure_or_invalid_output_never_runs_consumer(self):
        for result in (None, b"", b"not wasm"):
            with self.subTest(result=result), tempfile.TemporaryDirectory() as directory:
                error = subprocess.CalledProcessError if result is None else ValueError
                def build(*, env):
                    if result is None:
                        raise subprocess.CalledProcessError(1, "cargo")
                    artifact = Path(env["RUNNER_TEMP"]) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
                    artifact.parent.mkdir(parents=True)
                    artifact.write_bytes(result)

                run = mock.Mock()
                with mock.patch.object(probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]):
                    with self.assertRaises(error):
                        probe.experiment(Path(directory), env={"PATH": "/tools"}, build=build, run=run)
                run.assert_not_called()

    def test_bindgen_failure_prevents_node(self):
        def build(*, env):
            artifact = Path(env["RUNNER_TEMP"]) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"\x00asm\x01\x00\x00\x00")

        run = mock.Mock(side_effect=subprocess.CalledProcessError(1, "bindgen"))
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]
        ):
            with self.assertRaises(subprocess.CalledProcessError):
                probe.experiment(Path(directory), env={"PATH": "/tools"}, build=build, run=run)
        self.assertEqual(run.call_count, 1)


if __name__ == "__main__":
    unittest.main()
