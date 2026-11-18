#!/usr/bin/env python3
"""Exercise the sealed input helper with a real ledger WASM consumer.

This compatibility probe does not authenticate the build or establish provenance.
Run it in CI after building the pinned ledger fixture and preparing Worker tools.
"""
import os
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_sealed_candidate as sealed


def main():
    if len(sys.argv) != 3:
        raise SystemExit("usage: check_worker_sealed_bindgen.py CANDIDATE OUTPUT_DIR")
    candidate_path, output_dir = map(Path, sys.argv[1:])
    bindgen = shutil.which("wasm-bindgen")
    node = shutil.which("node")
    if not bindgen or not node:
        raise SystemExit("wasm-bindgen and node must be available on PATH")
    with candidate_path.open("rb") as source:
        candidate = source.read(sealed.MAX_CANDIDATE_BYTES + 1)

    def verify(snapshot):
        if not snapshot.startswith(b"\x00asm\x01\x00\x00\x00"):
            raise ValueError("candidate is not a core WASM module")

    sealed.consume_sealed(
        candidate,
        verify,
        [os.path.abspath(bindgen), "--target", "nodejs", "--out-dir", str(output_dir),
         "--out-name", "nntune_api"],
        cwd=os.getcwd(),
        env=os.environ.copy(),
    )
    subprocess.run(
        [os.path.abspath(node), "-e", "require(process.argv[1]).run_ledger_tests()",
         str((output_dir / "nntune_api.js").resolve())],
        check=True,
    )


if __name__ == "__main__":
    main()
