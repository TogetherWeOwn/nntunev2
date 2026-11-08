#!/usr/bin/env python3
"""Unit tests for worker_tools.py. Run: python3 <this file>."""

from __future__ import annotations

import base64
import hashlib
import io
import json
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import worker_tools as wt

GOOD_ENV = {"GITHUB_ACTIONS": "true", "RUNNER_TEMP": "/t", "GITHUB_ENV": "/e", "GITHUB_PATH": "/p"}


def tar_with(name: str, data: bytes, kind=tarfile.REGTYPE, mode="w:gz") -> bytes:
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode=mode) as bundle:
        info = tarfile.TarInfo(name)
        info.type = kind
        info.size = len(data) if kind == tarfile.REGTYPE else 0
        if kind == tarfile.SYMTYPE:
            info.linkname = "/etc/passwd"
        bundle.addfile(info, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
    return buffer.getvalue()


class ManifestAndLocks(unittest.TestCase):
    def test_manifest_has_single_esbuild_version(self):
        pins = wt.load_pins()
        self.assertEqual(pins["runtime"]["esbuild"], "0.28.2")
        self.assertNotIn("version", pins["esbuild-npm"])
        self.assertTrue(pins["esbuild-npm"]["integrity"].startswith("sha512-"))

    def test_repository_locks_agree(self):
        wt.check_locks(wt.load_pins())

    def test_lock_mismatch_refused(self):
        pins = wt.load_pins()
        with tempfile.TemporaryDirectory() as tmp:
            for worker in wt.LOCKED_WORKERS:
                directory = Path(tmp) / "workers" / worker
                directory.mkdir(parents=True)
                (directory / "Cargo.lock").write_text(
                    '[[package]]\nname = "worker"\nversion = "0.8.7"\n'
                    '[[package]]\nname = "wasm-bindgen"\nversion = "0.2.129"\n')
            wt.check_locks(pins, Path(tmp))
            (Path(tmp) / "workers/mb/Cargo.lock").write_text(
                '[[package]]\nname = "worker"\nversion = "0.8.7"\n'
                '[[package]]\nname = "wasm-bindgen"\nversion = "0.2.128"\n')
            with self.assertRaises(ValueError):
                wt.check_locks(pins, Path(tmp))


class Refusals(unittest.TestCase):
    def test_accepts_actions_linux(self):
        wt.require_actions(GOOD_ENV, "linux", "x86_64")

    def test_outside_actions(self):
        with self.assertRaises(RuntimeError):
            wt.require_actions({**GOOD_ENV, "GITHUB_ACTIONS": "false"}, "linux", "x86_64")

    def test_wrong_platform(self):
        with self.assertRaises(RuntimeError):
            wt.require_actions(GOOD_ENV, "darwin", "x86_64")
        with self.assertRaises(RuntimeError):
            wt.require_actions(GOOD_ENV, "linux", "aarch64")

    def test_credentials_refused(self):
        for key in ("CLOUDFLARE_API_TOKEN", "CLOUDFLARE_ACCOUNT_ID", "CF_API_TOKEN"):
            with self.assertRaises(RuntimeError):
                wt.require_actions({**GOOD_ENV, key: "x"}, "linux", "x86_64")

    def test_install_refuses_before_work(self):
        calls = []
        with self.assertRaises(RuntimeError):
            wt.install({}, dl=lambda *a: calls.append(a), run=lambda *a, **k: calls.append(a))
        self.assertEqual(calls, [])


class Checksums(unittest.TestCase):
    def test_sha256(self):
        data = b"abc"
        self.assertEqual(wt.verified(data, hashlib.sha256(data).hexdigest()), data)
        with self.assertRaises(ValueError):
            wt.verified(data + b"x", hashlib.sha256(data).hexdigest())

    def test_integrity(self):
        data = b"abc"
        good = "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()
        self.assertEqual(wt.verified_integrity(data, good), data)
        with self.assertRaises(ValueError):
            wt.verified_integrity(b"abd", good)
        with self.assertRaises(ValueError):
            wt.verified_integrity(data, good.replace("sha512", "sha1"))

    def test_crate_tamper_and_index_mismatch(self):
        crate = b"crate"
        pin = {"version": "1.0.0", "sha256": hashlib.sha256(crate).hexdigest()}
        index = json.dumps({"vers": "1.0.0", "yanked": False, "cksum": pin["sha256"]}).encode()

        def make(body, idx):
            return lambda url, limit: idx if "index.crates.io" in url else body

        ran = []
        with tempfile.TemporaryDirectory() as tmp:
            wt.install_crate("tool-x", pin, make(crate, index), Path(tmp), Path(tmp) / "i",
                             run=lambda cmd, **k: ran.append(cmd))
        self.assertIn("--locked", ran[0])
        self.assertIn("=1.0.0", ran[0])
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_crate("tool-x", pin, make(b"tampered", index), Path(tmp), Path(tmp) / "i",
                             run=lambda *a, **k: self.fail("must not build"))
        bad = json.dumps({"vers": "1.0.0", "yanked": False, "cksum": "0" * 64}).encode()
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_crate("tool-x", pin, make(crate, bad), Path(tmp), Path(tmp) / "i",
                             run=lambda *a, **k: self.fail("must not build"))

    def pins_for(self, archive: bytes):
        return {"binaryen": {"version": "version_9", "asset": "b.tar.gz",
                             "sha256": hashlib.sha256(archive).hexdigest()}}

    def test_wasm_opt_tamper(self):
        archive = tar_with("binaryen-version_9/bin/wasm-opt", b"ELF")
        pins = self.pins_for(archive)
        sha = f"{pins['binaryen']['sha256']}  b.tar.gz".encode()

        def dl(body):
            return lambda url, limit: sha if url.endswith(".sha256") else body

        with tempfile.TemporaryDirectory() as tmp:
            target = wt.install_wasm_opt(pins, dl(archive), Path(tmp), Path(tmp) / "i")
            self.assertEqual(target.read_bytes(), b"ELF")
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_wasm_opt(pins, dl(archive + b"x"), Path(tmp), Path(tmp) / "i")
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_wasm_opt(pins, lambda u, l: b"0 other" if u.endswith(".sha256") else archive,
                                Path(tmp), Path(tmp) / "i")

    def test_wasm_opt_rejects_link_member(self):
        archive = tar_with("binaryen-version_9/bin/wasm-opt", b"", kind=tarfile.SYMTYPE)
        pins = self.pins_for(archive)
        sha = f"{pins['binaryen']['sha256']}  b.tar.gz".encode()
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_wasm_opt(pins, lambda u, l: sha if u.endswith(".sha256") else archive,
                                Path(tmp), Path(tmp) / "i")

    def test_esbuild(self):
        archive = tar_with("package/bin/esbuild", b"ELF")
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(archive).digest()).decode()
        pins = {"runtime": {"esbuild": "0.28.2"},
                "esbuild-npm": {"package": "@esbuild/linux-x64", "integrity": integrity}}
        urls = []

        def dl(url, limit):
            urls.append(url)
            return archive

        with tempfile.TemporaryDirectory() as tmp:
            target = wt.install_esbuild(pins, dl, Path(tmp), Path(tmp) / "i")
            self.assertEqual(target.read_bytes(), b"ELF")
        self.assertEqual(urls, ["https://registry.npmjs.org/@esbuild/linux-x64/-/linux-x64-0.28.2.tgz"])
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_esbuild(pins, lambda u, l: archive + b"x", Path(tmp), Path(tmp) / "i")
        wrong = tar_with("package/bin/other", b"ELF")
        pins["esbuild-npm"]["integrity"] = "sha512-" + base64.b64encode(
            hashlib.sha512(wrong).digest()).decode()
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(KeyError):
            wt.install_esbuild(pins, lambda u, l: wrong, Path(tmp), Path(tmp) / "i")

    def test_download_requires_https(self):
        with self.assertRaises(ValueError):
            wt.download("http://example.invalid/x", 10)


class CheckBuild(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        base = Path(self.tmp.name)
        self.cache = base / "cache"
        self.cache.mkdir()
        self.env = {
            "XDG_CACHE_HOME": str(self.cache),
            "WASM_BINDGEN_BIN": "/w/bin/wasm-bindgen",
            "ESBUILD_BIN": "/w/bin/esbuild",
            "WASM_OPT_BIN": "/w/bin/wasm-opt",
        }
        self.log = base / "build.log"
        self.lines = [
            "[INFO]: Using custom Wasm Bindgen from WASM_BINDGEN_BIN: /w/bin/wasm-bindgen",
            "[INFO]: Using custom Wasm Opt from WASM_OPT_BIN: /w/bin/wasm-opt",
            "[INFO]: Using custom Esbuild from ESBUILD_BIN: /w/bin/esbuild",
        ]

    def run_check(self, lines=None):
        self.log.write_text("\n".join(self.lines if lines is None else lines) + "\n")
        wt.check_build(str(self.log), self.env)

    def test_good(self):
        self.run_check()

    def test_ansi_emoji_and_crlf_log(self):
        coloured = ["\x1b[1m\x1b[2m[INFO]\x1b[0m: \u2699\ufe0f. " + l.split(": ", 1)[1] + "\r"
                    for l in self.lines]
        self.run_check(coloured)

    def test_missing_override(self):
        for i in range(3):
            with self.assertRaises(ValueError):
                self.run_check(self.lines[:i] + self.lines[i + 1:])

    def test_wrong_path(self):
        with self.assertRaises(ValueError):
            self.run_check([line.replace("/w/bin", "/x/bin") for line in self.lines])

    def test_fallback(self):
        warn = "[WARN]: WASM_OPT_BIN=/w not found, falling back to internal Wasm Opt implementation"
        with self.assertRaises(ValueError):
            self.run_check(self.lines + [warn])

    def test_download(self):
        with self.assertRaises(ValueError):
            self.run_check(self.lines + ["[INFO]: Downloading Esbuild@0.28.2..."])

    def test_cache_dirt(self):
        (self.cache / "worker-build").mkdir()
        self.run_check()  # an empty directory is fine
        (self.cache / "worker-build" / "esbuild-x").mkdir()
        with self.assertRaises(ValueError):
            self.run_check()

    def test_cache_unset_or_relative_override(self):
        with self.assertRaises(ValueError):
            wt.check_build(str(self.log), {**self.env, "XDG_CACHE_HOME": ""})
        self.log.write_text("\n".join(self.lines))
        with self.assertRaises(ValueError):
            wt.check_build(str(self.log), {**self.env, "ESBUILD_BIN": "esbuild"})


if __name__ == "__main__":
    unittest.main()
