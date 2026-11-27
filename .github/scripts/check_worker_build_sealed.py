#!/usr/bin/env python3
"""Experimental pinned ledger build to sealed bindgen input, not provenance.

Run only in Actions after preparing Worker tools. The build, copy, verifier,
consumer and environment are not authenticated by this compatibility probe.
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_rust_provenance as rust
import worker_sealed_candidate as sealed


def experiment(output_dir, *, env, build=rust.build_ledger, run=subprocess.run):
    path = env.get("PATH")
    if not path:
        raise ValueError("PATH is required in the probe environment")
    bindgen = shutil.which("wasm-bindgen", path=path)
    node = shutil.which("node", path=path)
    if not bindgen or not node:
        raise ValueError("wasm-bindgen and node must be available on PATH")

    def build_output(output):
        # build_ledger refuses an existing ledger-target and uses the pinned
        # compiler paths. Copying the result is not a proof of build provenance.
        build(env={**env, "RUNNER_TEMP": str(output.parent)})
        artifact = output.parent / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
        output.write_bytes(sealed.snapshot_build_output(artifact))

    def verify(snapshot):
        if not snapshot.startswith(b"\x00asm\x01\x00\x00\x00"):
            raise ValueError("candidate is not a core WASM module")

    output_dir = sealed.fresh_bindgen_output(output_dir)
    sealed.experiment_build_and_consume(
        build_output, verify,
        [os.path.abspath(bindgen), "--target", "nodejs", "--out-dir", str(output_dir),
         "--out-name", "nntune_api"], cwd=str(rust.ROOT), env=env, run=run,
    )
    run([os.path.abspath(node), "-e", "require(process.argv[1]).run_ledger_tests()",
         str(output_dir / "nntune_api.js")], env=env, check=True)


def main():
    if len(sys.argv) != 2:
        raise SystemExit("usage: check_worker_build_sealed.py OUTPUT_DIR")
    experiment(Path(sys.argv[1]), env=os.environ.copy())


if __name__ == "__main__":
    main()
