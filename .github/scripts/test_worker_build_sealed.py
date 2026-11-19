#!/usr/bin/env python3
"""Tests for the inert pinned-build to sealed-consumer experiment."""
import os
import shlex
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

    def test_probe_magic_check_does_not_validate_or_authenticate_candidate(self):
        # A truncated section passes the probe's header-only check. A mocked
        # consumer accepts it to demonstrate the verifier's limited guarantee.
        candidate = b"\x00asm\x01\x00\x00\x00\x01"

        def build(*, env):
            artifact = Path(env["RUNNER_TEMP"]) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(candidate)

        run = mock.Mock()
        with tempfile.TemporaryDirectory() as directory, mock.patch.object(
            probe.shutil, "which", side_effect=["/tools/wasm-bindgen", "/tools/node"]
        ):
            probe.experiment(Path(directory), env={"PATH": "/tools"}, build=build, run=run)
        self.assertEqual(run.call_count, 2)
        self.assertIn("pass_fds", run.call_args_list[0].kwargs)

    def test_probe_does_not_authenticate_path_consumers(self):
        # Real executables can ignore the sealed input and report success.
        def build(*, env):
            artifact = Path(env["RUNNER_TEMP"]) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"\x00asm\x01\x00\x00\x00\x01")

        with tempfile.TemporaryDirectory() as directory:
            tools = Path(directory) / "tools"
            tools.mkdir()
            marker = Path(directory) / "executed"
            for name in ("wasm-bindgen", "node"):
                executable = tools / name
                executable.write_text(f"#!/bin/sh\nprintf '%s\\n' {shlex.quote(name)} >> {shlex.quote(str(marker))}\n")
                executable.chmod(0o755)
            probe.experiment(Path(directory) / "output", env={"PATH": str(tools)}, build=build)
            self.assertEqual(marker.read_text().splitlines(), ["wasm-bindgen", "node"])
            self.assertFalse((Path(directory) / "output/nntune_api.js").exists())

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
