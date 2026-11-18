#!/usr/bin/env python3
"""Verify the pinned Worker Rust tuple; not wired into release builds.

Adapted from the Worker provenance draft; verified source identities are not
an allocation certificate or a binding between a build and an uploaded artifact.
"""
from __future__ import annotations

import hashlib
import io
import os
import pwd
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
# Loader overrides can replace code even when the compiler files match the pins.
RUST_ENV_PREFIXES = ("CARGO_TARGET_", "CARGO_PROFILE_", "CARGO_UNSTABLE_", "CARGO_BUILD_",
                     "LD_", "DYLD_")
RUST_ENV_ALLOWED = {"CARGO_BUILD_JOBS"}


def wasm_sections(module: bytes) -> list[tuple[int, bytes]]:
    """Frame bounded core-WASM sections for inspection, not module validation.

    This does not identify linker globals, authenticate bytes, or admit a build.
    """
    if not isinstance(module, bytes) or len(module) > 64 * 1024 * 1024:
        raise ValueError("invalid or oversized WASM module")
    if module[:8] != b"\0asm\x01\0\0\0":
        raise ValueError("expected a core WASM version 1 module")
    sections = []
    offset = 8
    while offset < len(module):
        if len(sections) >= 10_000:
            raise ValueError("too many WASM sections")
        section_id = module[offset]
        offset += 1
        size = 0
        for index in range(5):
            if offset == len(module):
                raise ValueError("truncated WASM section size")
            byte = module[offset]
            offset += 1
            if index == 4 and byte & 0xf0:
                raise ValueError("WASM section size exceeds u32")
            size |= (byte & 0x7f) << (7 * index)
            if not byte & 0x80:
                break
        else:
            raise ValueError("invalid WASM section size")
        if size > len(module) - offset:
            raise ValueError("truncated WASM section")
        sections.append((section_id, module[offset:offset + size]))
        offset += size
    return sections


def inspect_i32_globals(payload: bytes) -> list[tuple[bool, int]]:
    """Inspect a restricted global section; this is not module validation.

    Only direct i32.const initializers are supported. Indices are local to this
    section, not module global indices: imports and exports are not resolved.
    Nothing here authenticates a candidate or proves a linker's symbol meaning.
    """
    def leb(offset: int, signed: bool) -> tuple[int, int]:
        value = 0
        for index in range(5):
            if offset >= len(payload):
                raise ValueError("truncated global section integer")
            byte = payload[offset]
            offset += 1
            bits = byte & 0x7f
            if index == 4 and (bits > 15 if not signed else not (bits <= 7 or bits >= 0x78)):
                raise ValueError("global section integer exceeds 32 bits")
            value |= bits << (index * 7)
            if not byte & 0x80:
                if signed and byte & 0x40:
                    value -= 1 << ((index + 1) * 7)
                return value, offset
        raise ValueError("invalid global section integer")

    count, offset = leb(0, False)
    if count > 10_000:
        raise ValueError("too many WASM globals")
    result = []
    for _ in range(count):
        if len(payload) - offset < 3 or payload[offset] != 0x7f or payload[offset + 1] not in (0, 1):
            raise ValueError("unsupported global type or mutability")
        mutable = bool(payload[offset + 1])
        if payload[offset + 2] != 0x41:
            raise ValueError("unsupported global initializer")
        value, offset = leb(offset + 3, True)
        if offset >= len(payload) or payload[offset] != 0x0b:
            raise ValueError("unterminated global initializer")
        offset += 1
        result.append((mutable, value & 0xffff_ffff))
    if offset != len(payload):
        raise ValueError("trailing global section bytes")
    return result


def inspect_function_only_imports(payload: bytes) -> int:
    """Count imports only when all are functions; not module validation.

    This restricted inspection rejects global, memory, table and tag imports.
    It does not establish that the inspected bytes belong to a candidate build.
    """
    def u32(offset: int) -> tuple[int, int]:
        value = 0
        for index in range(5):
            if offset >= len(payload):
                raise ValueError("truncated import section integer")
            byte = payload[offset]
            offset += 1
            if index == 4 and byte & 0xf0:
                raise ValueError("import section integer exceeds u32")
            value |= (byte & 0x7f) << (7 * index)
            if not byte & 0x80:
                return value, offset
        raise ValueError("invalid import section integer")

    count, offset = u32(0)
    if count > 10_000:
        raise ValueError("too many WASM imports")
    for _ in range(count):
        for _ in range(2):
            length, offset = u32(offset)
            if length > len(payload) - offset:
                raise ValueError("truncated import name")
            try:
                payload[offset:offset + length].decode("utf-8")
            except UnicodeDecodeError as error:
                raise ValueError("invalid import name") from error
            offset += length
        if offset >= len(payload) or payload[offset] != 0:
            raise ValueError("only function imports are supported")
        _, offset = u32(offset + 1)
    if offset != len(payload):
        raise ValueError("trailing import section bytes")
    return count


def inspect_exports(payload: bytes) -> dict[str, tuple[int, int]]:
    """Decode bounded export descriptors without validating or authenticating a module.

    Indices remain module indices; resolving globals requires accounting for imports.
    """
    def u32(offset: int) -> tuple[int, int]:
        value = 0
        for index in range(5):
            if offset >= len(payload):
                raise ValueError("truncated export section integer")
            byte = payload[offset]
            offset += 1
            if index == 4 and byte & 0xf0:
                raise ValueError("export section integer exceeds u32")
            value |= (byte & 0x7f) << (7 * index)
            if not byte & 0x80:
                return value, offset
        raise ValueError("invalid export section integer")

    count, offset = u32(0)
    if count > 10_000:
        raise ValueError("too many WASM exports")
    exports = {}
    for _ in range(count):
        length, offset = u32(offset)
        if length > len(payload) - offset:
            raise ValueError("truncated export name")
        try:
            name = payload[offset:offset + length].decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError("invalid export name") from error
        offset += length
        if name in exports or offset >= len(payload) or payload[offset] not in (0, 1, 2, 3):
            raise ValueError("duplicate export or unsupported descriptor")
        kind = payload[offset]
        index, offset = u32(offset + 1)
        exports[name] = (kind, index)
    if offset != len(payload):
        raise ValueError("trailing export section bytes")
    return exports


def inspect_memory_section(payload: bytes) -> tuple[int, int | None]:
    """Inspect one defined, unshared wasm32 memory; not module validation.

    Does not exclude imported memories or authenticate the section's candidate.
    """
    def u32(offset: int) -> tuple[int, int]:
        value = 0
        for index in range(5):
            if offset >= len(payload):
                raise ValueError("truncated memory section integer")
            byte = payload[offset]
            offset += 1
            if index == 4 and byte & 0xf0:
                raise ValueError("memory section integer exceeds u32")
            value |= (byte & 0x7f) << (index * 7)
            if not byte & 0x80:
                return value, offset
        raise ValueError("invalid memory section integer")

    count, offset = u32(0)
    if count != 1:
        raise ValueError("expected one defined memory")
    if offset >= len(payload) or payload[offset] not in (0, 1):
        raise ValueError("unsupported memory limits")
    flags = payload[offset]
    initial, offset = u32(offset + 1)
    maximum = None
    if flags == 1:
        maximum, offset = u32(offset)
    if (offset != len(payload) or initial > 65536
            or (maximum is not None and not initial <= maximum <= 65536)):
        raise ValueError("invalid memory limits or trailing bytes")
    return initial, maximum


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
        if name in RUST_ENV_ALLOWED or (name == "CARGO_INCREMENTAL" and env[key] == "0"):
            continue
        if name == "CARGO_INCREMENTAL" or name in RUST_ENV_EXACT or name.startswith(RUST_ENV_PREFIXES):
            raise ValueError(f"{key} may change the pinned Rust build")
    if env.get("RUSTUP_HOME") and not Path(env["RUSTUP_HOME"]).is_absolute():
        raise ValueError("RUSTUP_HOME must be absolute")
    if env.get("RUSTUP_TOOLCHAIN") != version:
        raise ValueError("RUSTUP_TOOLCHAIN is not the pinned version")


def cargo_config_refusals(env, root: Path = ROOT) -> None:
    for key in ("HOME", "CARGO_HOME"):
        if env.get(key) and not Path(env[key]).is_absolute():
            raise ValueError(f"{key} must be absolute")
    home = Path(env.get("HOME") or pwd.getpwuid(os.getuid()).pw_dir) / ".cargo"
    if env.get("CARGO_HOME") and Path(env["CARGO_HOME"]).resolve() != home.resolve():
        raise ValueError("CARGO_HOME is not the default Cargo home")
    work = root / "workers"
    dirs = [home, *(d / ".cargo" for d in (*(work / w for w in LOCKED_WORKERS), work, *work.parents))]
    for directory in dirs:
        for name in ("config", "config.toml"):
            if os.path.lexists(directory / name):
                raise ValueError(f"Cargo config {directory / name} may change the pinned Rust build")


def load_std(env, rust: dict) -> dict:
    path = env.get("RUST_STD_ARCHIVE")
    if not path or not Path(path).is_file():
        raise ValueError("verified wasm32 std archive is missing; run worker_rust_provenance.py rust first")
    archive = Path(path)
    if archive.stat().st_size > 60_000_000:
        raise ValueError("wasm32 std archive exceeds the size limit")
    data = verified(archive.read_bytes(), rust["rust-std-wasm32"]["xz-sha256"])
    version, target = rust["version"], rust["target"]
    return tar_files(data, f"rust-std-{version}-{target}/rust-std-{target}/lib/rustlib/{target}/lib/", 200_000_000)


def parse_vv(text: str) -> dict:
    return dict(line.split(": ", 1) for line in text.splitlines() if ": " in line)


def tar_files(archive: bytes, prefix: str, limit: int) -> dict:
    """sha256 of each regular file below prefix in an xz tarball; nothing is extracted."""
    found = {}
    total = 0
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:xz") as bundle:
        for member in bundle:
            if not member.name.startswith(prefix):
                continue
            name = member.name[len(prefix):]
            if not name or member.isdir():
                continue
            if (not member.isfile() or name in found or any(p in ("", ".", "..") for p in name.split("/"))):
                raise ValueError("invalid or duplicate std archive member")
            total += member.size
            if total > limit:
                raise ValueError("std archive exceeds the unpacked size limit")
            found[name] = hashlib.sha256(bundle.extractfile(member).read()).hexdigest()
    if not found:
        raise ValueError("std archive contains no target library files")
    return found


def host_component_files(pins: dict, manifest_bytes: bytes, archives: dict) -> dict:
    """Derive host component digests only from the pinned channel and archives.

    This is not yet a build certificate: callers must also bind these files to
    the executables actually used and to the resulting artifact.
    """
    rust = pins["rust"]
    manifest = tomllib.loads(verified(manifest_bytes, rust["channel-manifest"]["sha256"]).decode())
    if set(archives) != {"rustc", "cargo", "rust-std"}:
        raise ValueError("missing or unexpected host components")
    files = {}
    for name in ("rustc", "cargo", "rust-std"):
        package = manifest["pkg"][name]
        if package["git_commit_hash"] != rust["commit"]:
            raise ValueError(f"channel manifest {name} commit disagrees with pin")
        target = package["target"][rust["host"]]
        data = archives[name]
        if len(data) > 200_000_000:
            raise ValueError("host component archive exceeds size limit")
        verified(data, target["xz_hash"])
        component = f"rust-std-{rust['host']}" if name == "rust-std" else name
        prefix = f"{name}-{rust['version']}-{rust['host']}/{component}/"
        entries = tar_files(data, prefix, 1_000_000_000)
        # Component-local rust-installer metadata, not a sysroot file.
        entries.pop("manifest.in", None)
        if files.keys() & entries.keys():
            raise ValueError("host component archives contain overlapping paths")
        files.update(entries)
    if not {"bin/rustc", "bin/cargo"}.issubset(files):
        raise ValueError("host component archives lack compiler or cargo")
    if not any(p.startswith(f"lib/rustlib/{rust['host']}/lib/") for p in files):
        raise ValueError("host component archives lack host std")
    return files


def download_host_component_files(pins: dict, dl=download) -> dict:
    """Fetch host components using only the authenticated channel manifest."""
    rust = pins["rust"]
    channel = rust["channel-manifest"]
    manifest_bytes = verified(dl(channel["url"], 5_000_000), channel["sha256"])
    manifest = tomllib.loads(manifest_bytes.decode())
    archives = {}
    for name in ("rustc", "cargo", "rust-std"):
        package = manifest["pkg"][name]
        if package["git_commit_hash"] != rust["commit"]:
            raise ValueError(f"channel manifest {name} commit disagrees with pin")
        target = package["target"][rust["host"]]
        archives[name] = dl(target["xz_url"], 200_000_000)
    return host_component_files(pins, manifest_bytes, archives)


def verify_host_component_files(sysroot: Path, expected: dict) -> None:
    """Compare installed host files to archive digests, without following links.

    This checks archive members only: extra files, executable selection and build
    inputs still need separate checks before this can certify a build.
    """
    if not expected or not sysroot.is_absolute() or not sysroot.is_dir():
        raise ValueError("missing host component files or invalid sysroot")
    if any(path.is_symlink() for path in (sysroot, *sysroot.parents)):
        raise ValueError("host sysroot path contains a symbolic link")
    for name, digest in expected.items():
        parts = name.split("/")
        if any(part in ("", ".", "..") for part in parts):
            raise ValueError("invalid host component path")
        current = sysroot
        for part in parts:
            current = current / part
            if current.is_symlink():
                raise ValueError("installed host component contains a symbolic link")
        if not current.is_file() or hashlib.sha256(current.read_bytes()).hexdigest() != digest:
            raise ValueError(f"installed host component differs from archive: {name}")


def verified_host_executables(sysroot: Path, expected: dict) -> tuple[Path, Path]:
    """Return explicit compiler paths only after checking their host components.

    Expected digests must come from host_component_files, not a caller assertion.
    This does not authenticate extra loader inputs, prevent check-to-use changes,
    or bind a subsequent build. A caller must invoke these paths directly in a
    separately guarded build.
    """
    if not {"bin/rustc", "bin/cargo"}.issubset(expected):
        raise ValueError("host components lack compiler or cargo")
    verify_host_component_files(sysroot, expected)
    rustc, cargo = sysroot / "bin/rustc", sysroot / "bin/cargo"
    if not os.access(rustc, os.X_OK) or not os.access(cargo, os.X_OK):
        raise ValueError("host compiler or cargo is not executable")
    return rustc, cargo


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
                         root: Path = ROOT, executables: tuple[Path, Path] | None = None) -> None:
    """Fail closed unless the active toolchain is exactly the pinned tuple.

    Installed wasm std is compared to the pinned archive (or injected test digests)."""
    rust = pins["rust"]
    rust_env_refusals(env, rust["version"])
    cargo_config_refusals(env, root)
    want = load_std(env, rust) if std_files is None else std_files

    def out(*cmd):
        return run(list(cmd), check=True, capture_output=True, text=True, cwd=ROOT, env=env).stdout

    rustc_bin, cargo_bin = (str(p) for p in executables) if executables else ("rustc", "cargo")
    rustc = parse_vv(out(rustc_bin, "-vV"))
    cargo = parse_vv(out(cargo_bin, "-vV"))
    if (rustc.get("release") != rust["version"] or rustc.get("commit-hash") != rust["commit"]
            or rustc.get("host") != rust["host"]):
        raise ValueError("active rustc is not the pinned release, commit and host")
    if cargo.get("host") != rust["host"] or cargo.get("commit-hash") != rust["cargo-commit"]:
        raise ValueError("active cargo is not the pinned cargo")
    sysroot = Path(out(rustc_bin, "--print", "sysroot").strip())
    if executables and sysroot != executables[0].parent.parent:
        raise ValueError("compiler reports a different sysroot")
    if not sysroot.is_absolute() or sysroot.name != f"{rust['version']}-{rust['host']}":
        raise ValueError("active sysroot is not the pinned absolute toolchain")
    libdir = sysroot / "lib/rustlib" / rust["target"] / "lib"
    if any(path.is_symlink() for path in (*sysroot.parents, sysroot, sysroot / "lib", sysroot / "lib/rustlib",
                                           sysroot / "lib/rustlib" / rust["target"], libdir)):
        raise ValueError("installed wasm32 std contains a symbolic link")
    if not libdir.is_dir():
        raise ValueError("pinned wasm32 std is not installed")
    files = list(libdir.rglob("*"))
    if any(f.is_symlink() for f in files):
        raise ValueError("installed wasm32 std contains a symbolic link")
    if any(not f.is_file() and not f.is_dir() for f in files):
        raise ValueError("installed wasm32 std contains a non-regular entry")
    have = {str(f.relative_to(libdir)): hashlib.sha256(f.read_bytes()).hexdigest()
            for f in files if f.is_file()}
    if not want or want != have:
        raise ValueError("installed wasm32 std differs from the verified official archive")


def install_rust(env=os.environ, dl=download, run=subprocess.run) -> None:
    require_actions(env)
    pins = load_pins()
    rust = pins["rust"]
    env = {**env, "RUSTUP_TOOLCHAIN": rust["version"]}
    rust_env_refusals(env, rust["version"])
    t, v = rust["target"], rust["version"]
    archive = verify_rust_source(pins, dl)
    std_files = tar_files(archive, f"rust-std-{v}-{t}/rust-std-{t}/lib/rustlib/{t}/lib/", 200_000_000)
    run(["rustup", "toolchain", "install", rust["version"], "--profile", "minimal",
         "--no-self-update", "--target", rust["target"]], check=True, cwd=ROOT, env=env)
    run(["rustup", "default", rust["version"]], check=True, cwd=ROOT, env=env)
    archive_path = Path(env["RUNNER_TEMP"]) / "rust-std.tar.xz"
    archive_path.write_bytes(archive)
    with open(env["GITHUB_ENV"], "a") as output:
        output.write(f"RUSTUP_TOOLCHAIN={rust['version']}\nRUST_STD_ARCHIVE={archive_path}\n")
    verify_rust_toolchain(pins, env, run, std_files)
    print(f"verified rust {rust['version']} {rust['commit']} {rust['host']} + {rust['target']}", flush=True)


def verify_rust_boundary(env=os.environ, dl=download, run=subprocess.run) -> tuple[Path, Path]:
    pins = load_pins()
    rust = pins["rust"]
    check_lock_identity(pins)
    rust_env_refusals(env, rust["version"])
    cargo_config_refusals(env)
    home = Path(env.get("HOME") or pwd.getpwuid(os.getuid()).pw_dir)
    rustup_home = Path(env.get("RUSTUP_HOME") or home / ".rustup")
    if not rustup_home.is_absolute():
        raise ValueError("RUSTUP_HOME must be absolute")
    sysroot = rustup_home / "toolchains" / f"{rust['version']}-{rust['host']}"
    executables = verified_host_executables(sysroot, download_host_component_files(pins, dl))
    verify_rust_toolchain(pins, env, run, executables=executables)
    return executables


def build_ledger(env=os.environ, dl=download, run=subprocess.run) -> None:
    """Build the synthetic ledger with the just-verified compiler paths.

    This is not a build certificate: loader inputs and check-to-use races remain.
    """
    require_actions(env)
    temp = Path(env["RUNNER_TEMP"])
    if not temp.is_absolute():
        raise ValueError("RUNNER_TEMP must be absolute")
    if os.path.lexists(temp / "ledger-target"):
        raise ValueError("ledger target directory must not already exist")
    # Reject known overrides before constructing a minimal child environment.
    # Host executables and loader inputs are still trusted, not authenticated.
    rust_env_refusals(env, load_pins()["rust"]["version"])
    cargo_config_refusals(env)
    child = {key: env[key] for key in ("HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN",
                                       "RUST_STD_ARCHIVE") if key in env}
    child.update({"PATH": "/usr/bin:/bin", "CARGO_INCREMENTAL": "0"})
    rustc, cargo = verify_rust_boundary(child, dl, run)
    run([str(cargo), "build", "--locked", "--release", "--manifest-path", "workers/api/Cargo.toml",
         "--target", "wasm32-unknown-unknown", "--features", "staging-body-wasm-test,ledger-wasm-test",
         "--target-dir", str(temp / "ledger-target")], check=True, cwd=ROOT, env={**child, "RUSTC": str(rustc)})
    artifact = temp / "ledger-target/wasm32-unknown-unknown/release/nntune_api.wasm"
    print(f"synthetic ledger wasm sha256 {hashlib.sha256(artifact.read_bytes()).hexdigest()}", flush=True)


if __name__ == "__main__":
    if sys.argv[1:] == ["rust"]:
        install_rust()
    elif sys.argv[1:] == ["verify-rust"]:
        verify_rust_boundary()
    elif sys.argv[1:] == ["build-ledger"]:
        build_ledger()
    elif sys.argv[1:] == ["verify-candidate-boundary"]:
        sys.exit("candidate WASM boundary unproved: no admission certificate can be issued")
    else:
        sys.exit("usage: worker_rust_provenance.py {rust|verify-rust|build-ledger|verify-candidate-boundary}")
