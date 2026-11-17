#!/usr/bin/env python3
"""Tests for the standalone Worker Rust provenance verifier."""
import hashlib
import io
import os
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
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

    def test_verified_host_executables_refuse_missing_or_changed_inputs(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "toolchain"
            (root / "bin").mkdir(parents=True)
            rustc, cargo = root / "bin/rustc", root / "bin/cargo"
            rustc.write_bytes(b"rustc")
            cargo.write_bytes(b"cargo")
            rustc.chmod(0o755)
            cargo.chmod(0o755)
            expected = {"bin/rustc": hashlib.sha256(b"rustc").hexdigest(),
                        "bin/cargo": hashlib.sha256(b"cargo").hexdigest()}
            self.assertEqual(wt.verified_host_executables(root, expected), (rustc, cargo))
            with self.assertRaises(ValueError):
                wt.verified_host_executables(root, {"bin/rustc": expected["bin/rustc"]})
            cargo.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                wt.verified_host_executables(root, expected)
            cargo.write_bytes(b"cargo")
            cargo.chmod(0o644)
            with self.assertRaises(ValueError):
                wt.verified_host_executables(root, expected)
            cargo.unlink()
            cargo.symlink_to(rustc)
            with self.assertRaises(ValueError):
                wt.verified_host_executables(root, expected)

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
        with tempfile.TemporaryDirectory() as tmp, patch.object(wt.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(Path(tmp) / "h"))):
            root = self.sysroot(tmp)
            (root / "lib/rustlib" / pins["rust"]["target"] / "lib/a.rlib").write_bytes(b"a")
            std = {"a.rlib": hashlib.sha256(b"a").hexdigest()}
            wt.verify_rust_toolchain(pins, env, self.fake_run(root), std, Path(tmp))
            for run in (self.fake_run(root, commit="0" * 40), self.fake_run(root, release="1.98.0"),
                        self.fake_run(Path(tmp) / "stable-x86_64-unknown-linux-gnu"),
                        self.fake_run(Path(root.name))):
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, run, std, Path(tmp))
            alias = Path(tmp) / "linked-parent"
            alias.symlink_to(Path(tmp), target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                wt.verify_rust_toolchain(pins, env, self.fake_run(alias / root.name), std, Path(tmp))

    def test_toolchain_probes_use_explicit_executables(self):
        pins = wt.load_pins()
        env = {"RUSTUP_TOOLCHAIN": pins["rust"]["version"]}
        with tempfile.TemporaryDirectory() as tmp, patch.object(wt.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(Path(tmp) / "h"))):
            root = self.sysroot(tmp)
            (root / "lib/rustlib" / pins["rust"]["target"] / "lib/a.rlib").write_bytes(b"a")
            paths = (root / "bin/rustc", root / "bin/cargo")
            fake = self.fake_run(root)
            calls = []
            def run(cmd, **kwargs):
                calls.append(cmd[0])
                return fake([Path(cmd[0]).name, *cmd[1:]], **kwargs)
            wt.verify_rust_toolchain(pins, env, run, {"a.rlib": hashlib.sha256(b"a").hexdigest()},
                                     Path(tmp), paths)
            self.assertEqual(calls, [str(paths[0]), str(paths[1]), str(paths[0])])
            fake = self.fake_run(Path(tmp) / "other")
            with self.assertRaisesRegex(ValueError, "different sysroot"):
                wt.verify_rust_toolchain(pins, env, run, {"a.rlib": hashlib.sha256(b"a").hexdigest()},
                                         Path(tmp), paths)

    def test_toolchain_probes_use_validated_environment(self):
        pins = wt.load_pins()
        env = {"RUSTUP_TOOLCHAIN": pins["rust"]["version"]}
        with tempfile.TemporaryDirectory() as tmp, patch.object(wt.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(Path(tmp) / "h"))):
            root = self.sysroot(tmp)
            (root / "lib/rustlib" / pins["rust"]["target"] / "lib/a.rlib").write_bytes(b"a")
            fake = self.fake_run(root)

            def run(cmd, **kwargs):
                self.assertEqual(kwargs["env"], env)
                return fake(cmd, **kwargs)

            with patch.dict("os.environ", {"RUSTUP_TOOLCHAIN": "stable"}):
                wt.verify_rust_toolchain(pins, env, run, {"a.rlib": hashlib.sha256(b"a").hexdigest()}, Path(tmp))

    def test_environment_overrides_refused(self):
        for bad in ({}, {"RUSTUP_TOOLCHAIN": "stable"}, {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTFLAGS": "-C x"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_TARGET": "x86_64-unknown-linux-gnu"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS": "x"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_INCREMENTAL": "1"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTC_WRAPPER": "w"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "LD_PRELOAD": "/tmp/library.so"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "LD_LIBRARY_PATH": "/tmp/lib"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "DYLD_INSERT_LIBRARIES": "/tmp/library.dylib"},
                    {"RUSTUP_TOOLCHAIN": "1.98.1", "RUSTUP_DIST_SERVER": "x", "RUSTUP_UPDATE_ROOT": "x"}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                wt.rust_env_refusals(bad, "1.98.1")
        wt.rust_env_refusals({"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_BUILD_JOBS": "2", "CARGO_INCREMENTAL": "0"}, "1.98.1")

    def test_installed_std_must_match_verified_digests(self):
        pins = wt.load_pins()
        rust = pins["rust"]
        with tempfile.TemporaryDirectory() as tmp:
            root = self.sysroot(tmp)
            lib = root / "lib/rustlib" / rust["target"] / "lib"
            archive = Path(tmp) / "std.tar.xz"
            prefix = f"rust-std-{rust['version']}-{rust['target']}/rust-std-{rust['target']}/lib/rustlib/{rust['target']}/lib/"
            with tarfile.open(archive, "w:xz") as bundle:
                entry = tarfile.TarInfo(prefix + "libcore.rlib")
                entry.size = 4
                bundle.addfile(entry, io.BytesIO(b"core"))
            rust["rust-std-wasm32"]["xz-sha256"] = hashlib.sha256(archive.read_bytes()).hexdigest()
            env = {"RUSTUP_TOOLCHAIN": "1.98.1", "CARGO_HOME": str(Path(tmp) / ".cargo"), "RUST_STD_ARCHIVE": str(archive)}
            (lib / "libcore.rlib").write_bytes(b"core")
            with patch.object(wt.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=tmp)):
                wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").write_bytes(b"tampered")
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").unlink()
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").write_bytes(b"core")
                extra = lib / "unexpected.rlib"
                extra.write_bytes(b"extra")
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                extra.unlink()
                (lib / "libcore.rlib").unlink()
                external = Path(tmp) / "external.rlib"
                external.write_bytes(b"core")
                (lib / "libcore.rlib").symlink_to(external)
                with self.assertRaisesRegex(ValueError, "symbolic link"):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                (lib / "libcore.rlib").unlink()
                (lib / "libcore.rlib").write_bytes(b"core")
                moved = lib.with_name("actual-lib")
                lib.rename(moved)
                lib.symlink_to(moved, target_is_directory=True)
                with self.assertRaisesRegex(ValueError, "symbolic link"):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                lib.unlink()
                moved.rename(lib)
                target_dir = lib.parent
                moved_target = target_dir.with_name("actual-target")
                target_dir.rename(moved_target)
                target_dir.symlink_to(moved_target, target_is_directory=True)
                with self.assertRaisesRegex(ValueError, "symbolic link"):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                target_dir.unlink()
                moved_target.rename(target_dir)
                fifo = lib / "unexpected-fifo"
                os.mkfifo(fifo)
                with self.assertRaisesRegex(ValueError, "non-regular entry"):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                fifo.unlink()
                archive.write_bytes(b"forged")
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                archive.unlink()
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))
                env.pop("RUST_STD_ARCHIVE")
                env["RUST_STD_DIGESTS"] = str(Path(tmp) / "forged.json")
                with self.assertRaises(ValueError):
                    wt.verify_rust_toolchain(pins, env, self.fake_run(root), None, Path(tmp))

    def test_cargo_config_and_home_refused(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(wt.pwd, "getpwuid", return_value=SimpleNamespace(pw_dir=str(Path(tmp) / "h"))):
            tmp = Path(tmp)
            for directory in (tmp / "repo/workers/api", tmp / "repo/workers", tmp, tmp / "h"):
                config = directory / ".cargo/config.toml"
                config.parent.mkdir(parents=True, exist_ok=True)
                config.touch()
                with self.assertRaises(ValueError):
                    wt.cargo_config_refusals({}, tmp / "repo")
                config.unlink()
            for env in ({"CARGO_HOME": str(tmp / "other")}, {"HOME": "child"}, {"CARGO_HOME": ".cargo"}):
                with self.assertRaises(ValueError):
                    wt.cargo_config_refusals(env, tmp / "repo")
            wt.cargo_config_refusals({"HOME": ""}, tmp / "repo")

    def test_supplied_home_config_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp, patch.dict("os.environ", {"HOME": str(Path(tmp) / "ambient")}):
            home = Path(tmp) / "child"
            (home / ".cargo").mkdir(parents=True)
            (home / ".cargo/config.toml").touch()
            with self.assertRaises(ValueError):
                wt.cargo_config_refusals({"HOME": str(home)}, Path(tmp) / "repo")

    def test_install_and_probes_share_pinned_environment(self):
        pins = wt.load_pins()
        with tempfile.TemporaryDirectory() as tmp:
            root = self.sysroot(tmp)
            (root / "lib/rustlib" / pins["rust"]["target"] / "lib/a.rlib").write_bytes(b"a")
            env = {"HOME": tmp, "RUNNER_TEMP": tmp, "GITHUB_ENV": str(Path(tmp) / "github-env"), "RUSTUP_HOME": str(Path(tmp) / "child")}
            expected = {**env, "RUSTUP_TOOLCHAIN": pins["rust"]["version"]}
            calls = []
            fake = self.fake_run(root)
            def run(cmd, **kwargs):
                calls.append(cmd[0])
                self.assertEqual(kwargs["env"], expected)
                return fake(cmd, **kwargs)
            with patch.dict("os.environ", {"RUSTUP_HOME": str(Path(tmp) / "ambient")}), patch.object(wt, "require_actions"), patch.object(wt, "verify_rust_source", return_value=b"verified archive"), patch.object(wt, "tar_files", return_value={"a.rlib": hashlib.sha256(b"a").hexdigest()}):
                wt.install_rust(env, run=run)
            self.assertEqual(calls, ["rustup", "rustup", "rustc", "cargo", "rustc"])

    def test_relative_rustup_home_refused_before_install_or_probe(self):
        env = {"RUSTUP_HOME": "relative", "RUSTUP_TOOLCHAIN": "1.98.1"}
        def no_run(*args, **kwargs):
            self.fail("subprocess must not run")
        with self.assertRaisesRegex(ValueError, "RUSTUP_HOME must be absolute"):
            wt.verify_rust_toolchain(wt.load_pins(), env, no_run, {})
        with patch.object(wt, "require_actions"), self.assertRaisesRegex(ValueError, "RUSTUP_HOME must be absolute"):
            wt.install_rust(env, run=no_run)

    def test_api_lock_identity_is_pinned(self):
        pins = wt.load_pins()
        wt.check_lock_identity(pins)
        pins["locks"]["workers/api/Cargo.lock"] = "0" * 64
        with self.assertRaises(ValueError):
            wt.check_lock_identity(pins)
        pins["locks"] = {}
        with self.assertRaises(ValueError):
            wt.check_lock_identity(pins)

    def test_candidate_checks_host_components_before_explicit_probes(self):
        pins = wt.load_pins()
        env = {"HOME": "/home/build", "RUSTUP_TOOLCHAIN": pins["rust"]["version"]}
        root = Path("/home/build/.rustup/toolchains") / f"{pins['rust']['version']}-{pins['rust']['host']}"
        paths = (root / "bin/rustc", root / "bin/cargo")
        with patch.object(wt, "load_pins", return_value=pins), \
             patch.object(wt, "check_lock_identity"), patch.object(wt, "cargo_config_refusals"), \
             patch.object(wt, "download_host_component_files", return_value={"verified": "digest"}) as fetch, \
             patch.object(wt, "verified_host_executables", return_value=paths) as host, \
             patch.object(wt, "verify_rust_toolchain") as probe:
            wt.verify_rust_boundary(env)
            fetch.assert_called_once()
            host.assert_called_once_with(root, {"verified": "digest"})
            self.assertEqual(probe.call_args.kwargs["executables"], paths)
            host.side_effect = ValueError("changed host")
            probe.reset_mock()
            with self.assertRaisesRegex(ValueError, "changed host"):
                wt.verify_rust_boundary(env)
            probe.assert_not_called()

    def test_ledger_build_uses_verified_paths_and_refuses_reused_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = {"RUNNER_TEMP": tmp}
            rustc, cargo = Path("/pinned/bin/rustc"), Path("/pinned/bin/cargo")
            artifact = Path(tmp) / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
            calls = []

            def run(cmd, **kwargs):
                calls.append((cmd, kwargs))
                artifact.parent.mkdir(parents=True)
                artifact.write_bytes(b"wasm")

            with patch.object(wt, "require_actions"), \
                 patch.object(wt, "verify_rust_boundary", return_value=(rustc, cargo)) as verify:
                wt.build_ledger(env, run=run)
                self.assertEqual(calls[0][0][0], str(cargo))
                self.assertEqual(calls[0][0], [str(cargo), "build", "--locked", "--release", "--manifest-path",
                                              "workers/api/Cargo.toml", "--target", "wasm32-unknown-unknown",
                                              "--features", "staging-body-wasm-test,ledger-wasm-test",
                                              "--target-dir", str(Path(tmp) / "ledger-target")])
                self.assertEqual(calls[0][1]["env"]["RUSTC"], str(rustc))
                with self.assertRaisesRegex(ValueError, "must not already exist"):
                    wt.build_ledger(env, run=run)
                self.assertEqual(verify.call_count, 1)
                self.assertEqual(len(calls), 1)

        with patch.object(wt, "require_actions"), \
             patch.object(wt, "verify_rust_boundary", side_effect=ValueError("bad provenance")):
            with tempfile.TemporaryDirectory() as tmp, self.assertRaisesRegex(ValueError, "bad provenance"):
                wt.build_ledger({"RUNNER_TEMP": tmp}, run=lambda *args, **kwargs: self.fail("build ran"))

    def test_candidate_verification_refuses_changed_lock_before_toolchain_probe(self):
        pins = wt.load_pins()
        pins["locks"]["workers/api/Cargo.lock"] = "0" * 64
        with patch.object(wt, "load_pins", return_value=pins), patch.object(wt, "verify_rust_toolchain") as probe:
            with self.assertRaises(ValueError):
                wt.verify_rust_boundary()
            probe.assert_not_called()

    def test_std_archive_refuses_ambiguous_or_incomplete_members(self):
        def archive(entries):
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:xz") as bundle:
                for name, kind, data in entries:
                    entry = tarfile.TarInfo("lib/" + name)
                    entry.type = kind
                    if kind == tarfile.REGTYPE:
                        entry.size = len(data)
                        bundle.addfile(entry, io.BytesIO(data))
                    else:
                        entry.linkname = "target"
                        bundle.addfile(entry)
            return output.getvalue()

        regular = tarfile.REGTYPE
        cases = [[], [("a", regular, b"a"), ("a", regular, b"b")],
                 [("../a", regular, b"a")], [("a", tarfile.SYMTYPE, b"")],
                 [("a", regular, b"12"), ("b", regular, b"34")]]
        for entries in cases:
            with self.subTest(entries=entries), self.assertRaises(ValueError):
                wt.tar_files(archive(entries), "lib/", 3)
        self.assertEqual(wt.tar_files(archive([("a", regular, b"a")]), "lib/", 3),
                         {"a": hashlib.sha256(b"a").hexdigest()})

    def test_host_component_digests_require_pinned_manifest_and_archives(self):
        pins = wt.load_pins()
        rust = pins["rust"]
        version, host = rust["version"], rust["host"]
        archives = {}
        paths = {"rustc": "bin/rustc", "cargo": "bin/cargo",
                 "rust-std": f"lib/rustlib/{host}/lib/libstd.rlib"}
        for name, path in paths.items():
            component = f"rust-std-{host}" if name == "rust-std" else name
            prefix = f"{name}-{version}-{host}/{component}/"
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:xz") as bundle:
                entry = tarfile.TarInfo(prefix + path)
                entry.size = len(name)
                bundle.addfile(entry, io.BytesIO(name.encode()))
                metadata = tarfile.TarInfo(prefix + "manifest.in")
                metadata.size = len(name)
                bundle.addfile(metadata, io.BytesIO(name.encode()))
            archives[name] = output.getvalue()
        manifest = "".join(
            f'[pkg.{name}]\ngit_commit_hash = "{rust["commit"]}"\n'
            f'[pkg.{name}.target.{host}]\nxz_hash = "{hashlib.sha256(archives[name]).hexdigest()}"\n'
            f'xz_url = "https://example.invalid/{name}"\n'
            for name in paths
        ).encode()
        rust["channel-manifest"]["sha256"] = hashlib.sha256(manifest).hexdigest()
        result = wt.host_component_files(pins, manifest, archives)
        self.assertEqual(set(result), set(paths.values()))
        urls = {rust["channel-manifest"]["url"]: manifest,
                **{f"https://example.invalid/{name}": data for name, data in archives.items()}}
        self.assertEqual(wt.download_host_component_files(pins, lambda url, limit: urls[url]), result)
        with self.assertRaises(ValueError):
            wt.download_host_component_files(pins, lambda url, limit: b"tampered")
        bad = {**urls, "https://example.invalid/cargo": b"tampered"}
        with self.assertRaises(ValueError):
            wt.download_host_component_files(pins, lambda url, limit: bad[url])
        with self.assertRaises(ValueError):
            wt.host_component_files(pins, manifest + b" ", archives)
        for name in paths:
            with self.subTest(name=name), self.assertRaises(ValueError):
                wt.host_component_files(pins, manifest, {**archives, name: b"tampered"})
        with self.assertRaises(ValueError):
            wt.host_component_files(pins, manifest, {"rustc": archives["rustc"]})
        with self.assertRaises(ValueError):
            wt.host_component_files(pins, manifest, {**archives, "extra": b""})

    def test_host_files_must_match_and_cannot_follow_links(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "toolchain"
            binary = root / "bin/rustc"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"rustc")
            expected = {"bin/rustc": hashlib.sha256(b"rustc").hexdigest()}
            wt.verify_host_component_files(root, expected)
            alias = Path(tmp) / "alias"
            alias.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                wt.verify_host_component_files(alias, expected)
            parent_alias = Path(tmp) / "parent-alias"
            parent_alias.symlink_to(Path(tmp), target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                wt.verify_host_component_files(parent_alias / root.name, expected)
            with self.assertRaises(ValueError):
                wt.verify_host_component_files(Path("relative"), expected)
            binary.write_bytes(b"changed")
            with self.assertRaises(ValueError):
                wt.verify_host_component_files(root, expected)
            binary.unlink()
            with self.assertRaises(ValueError):
                wt.verify_host_component_files(root, expected)
            external = Path(tmp) / "external"
            external.write_bytes(b"rustc")
            binary.symlink_to(external)
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                wt.verify_host_component_files(root, expected)
            binary.unlink()
            binary.parent.rmdir()
            external_dir = Path(tmp) / "external-bin"
            external_dir.mkdir()
            (external_dir / "rustc").write_bytes(b"rustc")
            binary.parent.symlink_to(external_dir, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symbolic link"):
                wt.verify_host_component_files(root, expected)
            for invalid in ({}, {"../external": "x"}, {"/external": "x"}):
                with self.assertRaises(ValueError):
                    wt.verify_host_component_files(root, invalid)

    def test_source_refuses_tampered_manifest(self):
        with self.assertRaises(ValueError):
            wt.verify_rust_source(wt.load_pins(), lambda url, limit: b"tampered")


if __name__ == "__main__":
    unittest.main()
