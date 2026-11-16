#!/usr/bin/env python3
"""Tests for the standalone Worker Rust provenance verifier."""
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import worker_rust_provenance as wt

class RustPin(unittest.TestCase):
    VV = ("rustc 1.98.1\nbinary: rustc\ncommit-hash: {c}\nhost: {h}\nrelease: {r}\n")

    def fake_run(self, sysroot, commit=None, release=None):
        rust = wt.load_pins()["rust"]

        def run(cmd, **kwargs):
            class Result:
                stdout = ""
            if cmd[0] == "rustc" and cmd[1] == "-vV":
                Result.stdout = self.VV.format(c=commit or rust["commit"], h=rust["host"],
                                               r=release or rust["version"])
            elif cmd[0] == "cargo":
                Result.stdout = self.VV.format(c=rust["cargo-commit"], h=rust["host"], r="1.98.1")
            else:
                Result.stdout = str(sysroot) + "\n"
            return Result
        return run

    def sysroot(self, tmp):
        rust = wt.load_pins()["rust"]
        root = Path(tmp) / f"{rust['version']}-{rust['host']}"
        (root / "lib/rustlib" / rust["target"] / "lib").mkdir(parents=True)
        return root

    def test_pins_are_the_verified_official_tuple(self):
        rust = wt.load_pins()["rust"]
        self.assertEqual(rust["commit"], "48a229ceaefd4985c50990b14116b6d856af0985")
        self.assertEqual(rust["channel-manifest"]["sha256"],
                         "a7c8774a5fd8441c997d94c029776cbc5eb111e9d72ab5d256fa69866644347e")
        self.assertEqual(rust["rust-src"]["xz-sha256"],
                         "5c846ebcebcc7e2e0777a4cdaa12051691593f16a7e94edbae5e6241cc62d98c")

    def test_toolchain_accepts_only_the_tuple(self):
        pins = wt.load_pins()
        env = {"RUSTUP_TOOLCHAIN": "1.98.1"}
        with tempfile.TemporaryDirectory() as tmp, patch.object(Path, "home", return_value=Path(tmp) / "h"):
            root = self.sysroot(tmp)
            (root / "lib/rustlib" / pins["rust"]["target"] / "lib/a.rlib").write_bytes(b"a")
            std = {"a.rlib": hashlib.sha256(b"a").hexdigest()}
            wt.verify_rust_toolchain(pins, env, self.fake_run(root), std, Path(tmp))
            for run in (self.fake_run(root, commit="0" * 40), self.fake_run(root, release="1.98.0"),
                        self.fake_run(Path(tmp) / "stable-x86_64-unknown-linux-gnu")):
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, run, std, Path(tmp))

    def test_environment_overrides_refused(self):
        for bad in ({}, {"RUSTUP_TOOLCHAIN": "stable"}, {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTFLAGS": "-C x"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_TARGET": "x86_64-unknown-linux-gnu"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS": "x"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTC_WRAPPER": "w"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTUP_DIST_SERVER": "x", "RUSTUP_UPDATE_ROOT": "x"}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                wt.rust_env_refusals(bad, "1.98.1")
        wt.rust_env_refusals({"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_JOBS": "2"}, "1.98.1")

    def test_installed_std_must_match_verified_digests(self):
        pins = wt.load_pins()
        rust = pins["rust"]
        with tempfile.TemporaryDirectory() as tmp:
            root = self.sysroot(tmp)
            lib = root / "lib/rustlib" / rust["target"] / "lib"
            digests = Path(tmp) / "digests.json"
            env = {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_HOME": str(Path(tmp) / ".cargo"), "RUST_STD_DIGESTS": str(digests)}
            (lib / "libcore.rlib").write_bytes(b"core")
            digests.write_text(json.dumps({"libcore.rlib": hashlib.sha256(b"core").hexdigest()}))
            with patch.object(Path, "home", return_value=Path(tmp)):
                wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").write_bytes(b"tampered")
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").write_bytes(b"core")
                digests.unlink()
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))

    def test_cargo_config_and_home_refused(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(Path, "home", return_value=Path(tmp) / "h"):
            tmp = Path(tmp)
            for directory in (tmp / "repo/workers/api", tmp / "repo/workers", tmp, tmp / "h"):
                config = directory / ".cargo/config.toml"
                config.parent.mkdir(parents=True, exist_ok=True)
                config.touch()
                with self.assertRaises(ValueError):
                    wt.cargo_config_refusals({}, tmp / "repo")
                config.unlink()
            with self.assertRaises(ValueError):
                wt.cargo_config_refusals({"CARGO_HOME": str(tmp / "other")}, tmp / "repo")

    def test_api_lock_identity_is_pinned(self):
        pins = wt.load_pins()
        wt.check_lock_identity(pins)
        pins["locks"]["workers/api/Cargo.lock"] = "0" * 64
        with self.assertRaises(ValueError):
            wt.check_lock_identity(pins)
        pins["locks"] = {}
        with self.assertRaises(ValueError):
            wt.check_lock_identity(pins)

    def test_source_refuses_tampered_manifest(self):
        with self.assertRaises(ValueError):
            wt.verify_rust_source(wt.load_pins(), lambda url, limit: b"tampered")


if __name__ == "__main__":
    unittest.main()
