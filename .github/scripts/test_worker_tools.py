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
from unittest import mock

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

    def test_profile_selection_and_cli(self):
        manifest = wt.load_pins()
        ordinary, workers = wt.select_profile(manifest, "ordinary")
        self.assertEqual(workers, wt.LOCKED_WORKERS)
        self.assertEqual(ordinary["worker-build"]["version"], "0.8.7")
        repair, workers = wt.select_profile(manifest, "repair")
        self.assertEqual(workers, ("repair",))
        self.assertEqual(repair["worker-build"], {
            "version": "0.9.0", "sha256": "bdcd75703783317d833b09c6724d5e4f642ad23804d8d14e3ac362e990c7cab7"})
        self.assertEqual(repair["wasm-bindgen-cli"], {
            "version": "0.2.130", "sha256": "c37f6904387d19a319525c74af274c73912f3871c35555aa49f3ea64ebb2fc7a"})
        self.assertEqual(repair["binaryen"], ordinary["binaryen"])
        self.assertEqual(repair["esbuild-npm"], ordinary["esbuild-npm"])
        with self.assertRaises(ValueError):
            wt.select_profile(manifest, "unknown")
        with (mock.patch.object(wt, "install") as install,
              mock.patch("worker_rust_provenance.check_lock_identity") as check_lock,
              mock.patch("worker_rust_provenance.verify_rust_toolchain") as verify_rust):
            self.assertEqual(wt.main(["worker_tools.py", "install"]), 0)
            install.assert_called_once_with()
            check_lock.assert_called_once_with(manifest)
            verify_rust.assert_called_once_with(manifest)
            install.reset_mock()
            self.assertEqual(wt.main(["worker_tools.py", "install", "repair"]), 0)
            install.assert_called_once_with(profile="repair")
            self.assertEqual(wt.main(["worker_tools.py", "install", "unknown"]), 2)
            check_lock.assert_called_once()
            verify_rust.assert_called_once()
            with mock.patch.object(wt, "check_build") as check_build:
                self.assertEqual(wt.main(["worker_tools.py", "check-build", "log"]), 0)
                self.assertEqual(wt.main(["worker_tools.py", "check-build", "log", "repair"]), 0)
                self.assertEqual(wt.main(["worker_tools.py", "check-build", "log", "unknown"]), 2)
                self.assertEqual(check_build.call_count, 2)
                self.assertEqual(check_lock.call_count, 2)
                self.assertEqual(verify_rust.call_count, 2)

    def test_repair_lock_selection_and_mismatch(self):
        pins, workers = wt.select_profile(wt.load_pins(), "repair")
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            directory = root / "workers/repair"
            directory.mkdir(parents=True)
            lock = directory / "Cargo.lock"
            def write_lock(worker, bindgen):
                lock.write_text(f'[[package]]\nname = "worker"\nversion = "{worker}"\n'
                                f'[[package]]\nname = "wasm-bindgen"\nversion = "{bindgen}"\n')
            write_lock("0.9.0", "0.2.130")
            wt.check_locks(pins, root, workers)
            for worker, bindgen in (("0.8.7", "0.2.130"), ("0.9.0", "0.2.129")):
                write_lock(worker, bindgen)
                with self.assertRaises(ValueError):
                    wt.check_locks(pins, root, workers)
            lock.write_text('[[package]]\nname = "worker"\nversion = "0.9.0"\n')
            with self.assertRaises(ValueError):
                wt.check_locks(pins, root, workers)

    def test_install_passes_selected_pins_to_verified_route(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            env = {**GOOD_ENV, "RUNNER_TEMP": tmp,
                   "GITHUB_ENV": str(root / "env"), "GITHUB_PATH": str(root / "path")}
            installed = []
            def crate(name, pin, dl, source, target, run):
                installed.append((name, pin))
            def executable(pins, dl, source, target):
                return target / "bin/tool"
            def run(cmd, **kwargs):
                version = {"wasm-bindgen": "0.2.130", "tool": "0.28.2 version 132",
                           "worker-build": "0.9.0"}[Path(cmd[0]).name]
                return mock.Mock(stdout=version)
            with mock.patch.object(wt, "require_actions"), mock.patch.object(wt, "check_locks") as locks, \
                 mock.patch.object(wt, "install_crate", side_effect=crate), \
                 mock.patch.object(wt, "install_wasm_opt", side_effect=executable), \
                 mock.patch.object(wt, "install_esbuild", side_effect=executable), \
                 mock.patch.object(wt.os, "access", return_value=True):
                wt.install(env, run=run, profile="repair")
            pins, workers = wt.select_profile(wt.load_pins(), "repair")
            locks.assert_called_once_with(pins, workers=workers)
            self.assertEqual(installed, [(name, pins[name]) for name in ("worker-build", "wasm-bindgen-cli")])
            self.assertIn("WASM_OPT_BIN=", (root / "env").read_text())

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

    def crate_archive(self, extra=(), lock=True, version="1.0.0"):
        files = [
            ("tool-x-1.0.0/Cargo.toml",
             f'[package]\nname = "tool-x"\nversion = "{version}"\n'.encode(), tarfile.REGTYPE),
            ("tool-x-1.0.0/src/main.rs", b"fn main() {}", tarfile.REGTYPE),
        ]
        if lock:
            files.append(("tool-x-1.0.0/Cargo.lock", b"version = 4\n", tarfile.REGTYPE))
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as bundle:
            for name, data, kind in [*files, *extra]:
                info = tarfile.TarInfo(name)
                info.type = kind
                info.size = len(data) if kind == tarfile.REGTYPE else 0
                if kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                    info.linkname = "Cargo.toml"
                bundle.addfile(info, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
        return buffer.getvalue()

    def install_archive(self, archive, root, run):
        pin = {"version": "1.0.0", "sha256": hashlib.sha256(archive).hexdigest()}
        index = json.dumps({"vers": "1.0.0", "yanked": False, "cksum": pin["sha256"]}).encode()
        downloads = []

        def dl(url, limit):
            downloads.append(url)
            return index if "index.crates.io" in url else archive

        wt.install_crate("tool-x", pin, dl, root, root / "installed", run=run)
        return downloads

    def test_cargo_installs_verified_source_with_shipped_lock(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            calls = []

            def run(cmd, **kwargs):
                calls.append(cmd)
                source = Path(cmd[cmd.index("--path") + 1])
                self.assertEqual(source, root / "tool-x/tool-x-1.0.0")
                self.assertEqual((source / "src/main.rs").read_bytes(), b"fn main() {}")
                self.assertEqual((source / "Cargo.lock").read_bytes(), b"version = 4\n")
                self.assertEqual(kwargs, {"cwd": wt.ROOT, "check": True})
                self.assertEqual(cmd, ["cargo", "install", "--path", str(source), "--locked",
                                       "--root", str(root / "installed")])

            downloads = self.install_archive(self.crate_archive(), root, run)
            self.assertEqual(len(calls), 1)
            self.assertEqual(len(downloads), 2)

    def test_crate_rejects_unsafe_members_before_cargo(self):
        for name, kind in (
            ("../escape", tarfile.REGTYPE),
            ("/absolute", tarfile.REGTYPE),
            ("tool-x-1.0.0/../escape", tarfile.REGTYPE),
            ("other-1.0.0/file", tarfile.REGTYPE),
            ("tool-x-1.0.0/link", tarfile.SYMTYPE),
            ("tool-x-1.0.0/link", tarfile.LNKTYPE),
            ("tool-x-1.0.0/pipe", tarfile.FIFOTYPE),
            ("tool-x-1.0.0/Cargo.toml", tarfile.REGTYPE),
        ):
            with self.subTest(name=name, kind=kind), tempfile.TemporaryDirectory() as tmp:
                with self.assertRaises(ValueError):
                    self.install_archive(self.crate_archive([(name, b"x", kind)]), Path(tmp),
                                         lambda *a, **k: self.fail("must not build"))

    def test_crate_requires_matching_identity_and_lock(self):
        for archive in (self.crate_archive(lock=False), self.crate_archive(version="2.0.0")):
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
                self.install_archive(archive, Path(tmp),
                                     lambda *a, **k: self.fail("must not build"))

    def test_crate_tamper_and_index_mismatch(self):
        crate = self.crate_archive()
        pin = {"version": "1.0.0", "sha256": hashlib.sha256(crate).hexdigest()}
        index = json.dumps({"vers": "1.0.0", "yanked": False, "cksum": pin["sha256"]}).encode()

        def make(body, idx):
            return lambda url, limit: idx if "index.crates.io" in url else body

        ran = []
        with tempfile.TemporaryDirectory() as tmp:
            wt.install_crate("tool-x", pin, make(crate, index), Path(tmp), Path(tmp) / "i",
                             run=lambda cmd, **k: ran.append(cmd))
        self.assertIn("--locked", ran[0])
        self.assertIn("--path", ran[0])
        self.assertNotIn("--version", ran[0])
        self.assertNotIn("tool-x", ran[0])
        with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
            wt.install_crate("tool-x", pin, make(b"tampered", index), Path(tmp), Path(tmp) / "i",
                             run=lambda *a, **k: self.fail("must not build"))
        for entry in ({"vers": "1.0.0", "yanked": False, "cksum": "0" * 64},
                      {"vers": "1.0.0", "yanked": True, "cksum": pin["sha256"]},
                      {"vers": "2.0.0", "yanked": False, "cksum": pin["sha256"]}):
            with tempfile.TemporaryDirectory() as tmp, self.assertRaises(ValueError):
                wt.install_crate("tool-x", pin, make(crate, json.dumps(entry).encode()),
                                 Path(tmp), Path(tmp) / "i",
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

    def test_cargo_registry_download_is_not_tool_download(self):
        self.run_check(self.lines + [" Downloading crates ...", "  Downloaded serde v1.0.228"])

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
