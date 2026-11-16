#!/usr/bin/env python3
"""Verify the pinned Worker Rust tuple; not wired into release builds.

Adapted from the Worker provenance draft; verified source identities are not
an allocation certificate or a binding between a build and an uploaded artifact.
"""
from __future__ import annotations

import hashlib
import io
import json
import os
import subprocess
import sys
import tarfile
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from worker_tools import ROOT, LOCKED_WORKERS, download, load_pins, require_actions, verified

# Environment that could silently change the compiler, std or codegen of a build.
RUST_ENV_EXACT = {"RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                  "RUSTC_BOOTSTRAP", "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS",
                  "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTC",
                  "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
                  "CARGO_BUILD_RUSTDOCFLAGS", "CARGO_BUILD_TARGET_DIR", "CARGO_TARGET_DIR",
                  "CARGO_UNSTABLE_BUILD_STD", "CARGO", "RUSTUP_HOME_OVERRIDE",
                  "RUSTUP_DIST_SERVER", "RUSTUP_UPDATE_ROOT"}
RUST_ENV_PREFIXES = ("CARGO_TARGET_", "CARGO_PROFILE_", "CARGO_UNSTABLE_", "CARGO_BUILD_")
RUST_ENV_ALLOWED = {"CARGO_BUILD_JOBS"}


def check_lock_identity(pins: dict, root: Path = ROOT) -> None:
    locks = pins.get("locks", {})
    if set(locks) != {"workers/api/Cargo.lock"}:
        raise ValueError("missing or unexpected pinned lock identities")
    for lock, digest in locks.items():
        if hashlib.sha256((root / lock).read_bytes()).hexdigest() != digest:
            raise ValueError(f"{lock} differs from the pinned lock identity")


def rust_env_refusals(env, version: str) -> None:
    """Refuse ambient settings that can alter the compiler, std or target."""
    for key in env:
        name = key.upper()
        if name in RUST_ENV_ALLOWED:
            continue
        if name in RUST_ENV_EXACT or name.startswith(RUST_ENV_PREFIXES):
            raise ValueError(f"{key} may change the pinned Rust build")
    if env.get("RUSTUP_TOOLCHAIN") != version:
        raise ValueError("RUSTUP_TOOLCHAIN is not the pinned version")


def cargo_config_refusals(env, root: Path = ROOT) -> None:
    home = Path.home() / ".cargo"
    if env.get("CARGO_HOME") and Path(env["CARGO_HOME"]).resolve() != home.resolve():
        raise ValueError("CARGO_HOME is not the default Cargo home")
    work = root / "workers"
    dirs = [home, *(d / ".cargo" for d in (*(work / w for w in LOCKED_WORKERS), work, *work.parents))]
    for directory in dirs:
        for name in ("config", "config.toml"):
            if os.path.lexists(directory / name):
                raise ValueError(f"Cargo config {directory / name} may change the pinned Rust build")


def load_std(env) -> dict:
    path = env.get("RUST_STD_DIGESTS")
    if not path or not Path(path).is_file():
        raise ValueError("verified wasm32 std digests are missing; run worker_rust_provenance.py rust first")
    return json.loads(Path(path).read_text())


def parse_vv(text: str) -> dict:
    return dict(line.split(": ", 1) for line in text.splitlines() if ": " in line)


def tar_files(archive: bytes, prefix: str, limit: int) -> dict:
    """sha256 of each regular file below prefix in an xz tarball; nothing is extracted."""
    found = {}
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:xz") as bundle:
        for member in bundle:
            if member.name.startswith(prefix) and member.isfile() and member.size <= limit:
                found[member.name[len(prefix):]] = hashlib.sha256(bundle.extractfile(member).read()).hexdigest()
    return found


def verify_rust_source(pins: dict, dl=download) -> bytes:
    """Check the official channel manifest, rust-src and wasm std against the pins.

    Returns the verified wasm std archive for comparison with the installed sysroot."""
    rust = pins["rust"]
    text = verified(dl(rust["channel-manifest"]["url"], 5_000_000), rust["channel-manifest"]["sha256"])
    manifest = tomllib.loads(text.decode())
    for name in ("rustc", "cargo", "rust-std", "rust-src"):
        if manifest["pkg"][name]["git_commit_hash"] != rust["commit"]:
            raise ValueError(f"channel manifest {name} commit disagrees with pin")
    if not manifest["pkg"]["rustc"]["version"].startswith(rust["version"] + " (" + rust["commit"][:9]):
        raise ValueError("channel manifest rustc version disagrees with pin")
    std = manifest["pkg"]["rust-std"]["target"][rust["target"]]
    src = manifest["pkg"]["rust-src"]["target"]["*"]
    for entry, component in ((std, "rust-std-wasm32"), (src, "rust-src")):
        if (entry["xz_url"] != rust[component]["xz-url"]
                or entry["xz_hash"] != rust[component]["xz-sha256"]):
            raise ValueError(f"channel manifest {component} disagrees with pin")
    source = verified(dl(rust["rust-src"]["xz-url"], 20_000_000), rust["rust-src"]["xz-sha256"])
    lock_path = f"rust-src-{rust['version']}/rust-src/lib/rustlib/src/rust/library/Cargo.lock"
    with tarfile.open(fileobj=io.BytesIO(source), mode="r:xz") as bundle:
        member = bundle.getmember(lock_path)
        if not member.isfile() or member.size > 2_000_000:
            raise ValueError("unexpected rust-src member")
        lock = tomllib.loads(bundle.extractfile(member).read().decode())
    want = rust["std-lock"]
    if not any(p["name"] == want["package"] and p["version"] == want["version"]
               and p.get("checksum") == want["checksum"] for p in lock["package"]):
        raise ValueError("std library lock disagrees with pinned allocator identity")
    return verified(dl(rust["rust-std-wasm32"]["xz-url"], 60_000_000), rust["rust-std-wasm32"]["xz-sha256"])


def verify_rust_toolchain(pins: dict, env=os.environ, run=subprocess.run, std_files: dict | None = None,
                         root: Path = ROOT) -> None:
    """Fail closed unless the active toolchain is exactly the pinned tuple.

    Installed wasm std is always compared to the verified digests (std_files or RUST_STD_DIGESTS)."""
    rust = pins["rust"]
    rust_env_refusals(env, rust["version"])
    cargo_config_refusals(env, root)
    want = load_std(env) if std_files is None else std_files

    def out(*cmd):
        return run(list(cmd), check=True, capture_output=True, text=True, cwd=ROOT).stdout

    rustc = parse_vv(out("rustc", "-vV"))
    cargo = parse_vv(out("cargo", "-vV"))
    if (rustc.get("release") != rust["version"] or rustc.get("commit-hash") != rust["commit"]
            or rustc.get("host") != rust["host"]):
        raise ValueError("active rustc is not the pinned release, commit and host")
    if cargo.get("host") != rust["host"] or cargo.get("commit-hash") != rust["cargo-commit"]:
        raise ValueError("active cargo is not the pinned cargo")
    sysroot = Path(out("rustc", "--print", "sysroot").strip())
    if sysroot.name != f"{rust['version']}-{rust['host']}":
        raise ValueError("active sysroot is not the pinned toolchain")
    libdir = sysroot / "lib/rustlib" / rust["target"] / "lib"
    if not libdir.is_dir():
        raise ValueError("pinned wasm32 std is not installed")
    have = {str(f.relative_to(libdir)): hashlib.sha256(f.read_bytes()).hexdigest()
            for f in libdir.rglob("*") if f.is_file()}
    if not want or want != have:
        raise ValueError("installed wasm32 std differs from the verified official archive")


def install_rust(env=os.environ, dl=download, run=subprocess.run) -> None:
    require_actions(env)
    pins = load_pins()
    rust = pins["rust"]
    rust_env_refusals({**env, "RUSTUP_TOOLCHAIN": rust["version"]}, rust["version"])
    t, v = rust["target"], rust["version"]
    std_files = tar_files(verify_rust_source(pins, dl), f"rust-std-{v}-{t}/rust-std-{t}/lib/rustlib/{t}/lib/", 200_000_000)
    run(["rustup", "toolchain", "install", rust["version"], "--profile", "minimal",
         "--no-self-update", "--target", rust["target"]], check=True, cwd=ROOT)
    run(["rustup", "default", rust["version"]], check=True, cwd=ROOT)
    digests = Path(env["RUNNER_TEMP"]) / "rust-std-digests.json"
    digests.write_text(json.dumps(std_files, sort_keys=True))
    with open(env["GITHUB_ENV"], "a") as output:
        output.write(f"RUSTUP_TOOLCHAIN={rust['version']}\nRUST_STD_DIGESTS={digests}\n")
    verify_rust_toolchain(pins, {**env, "RUSTUP_TOOLCHAIN": rust["version"]}, run, std_files)
    print(f"verified rust {rust['version']} {rust['commit']} {rust['host']} + {rust['target']}", flush=True)



if __name__ == "__main__":
    if sys.argv[1:] == ["rust"]:
        install_rust()
    elif sys.argv[1:] == ["verify-rust"]:
        verify_rust_toolchain(load_pins())
    else:
        sys.exit("usage: worker_rust_provenance.py {rust|verify-rust}")
