#!/usr/bin/env python3
"""Prepare and audit pinned worker build tools (GitHub Actions on Linux x86_64 only).

Usage:
  worker_tools.py rust                       install and verify the pinned Rust toolchain
  worker_tools.py install
  worker_tools.py check-build <worker-build-log>   (log named worker-<name>.log)

Pins live in .github/worker-toolchain.json, the single manifest. The install and
log-check logic is adapted from the earlier qualification tooling, reduced to what
ordinary api, mb and ingest release builds need.

worker-build 0.8.7 resolves each tool from a <NAME>_BIN override first, then its
cache, then a download. `check-build` proves every build used the overrides.
"""

from __future__ import annotations

import base64
import hashlib
import io
import json
import os
import platform
import re
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MANIFEST = ROOT / ".github/worker-toolchain.json"
LOCKED_WORKERS = ("api", "mb", "ingest")
CREDENTIAL_PREFIXES = ("CLOUDFLARE_", "CF_")
ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
# worker-build 0.8.7 full names, as printed in "Using custom <name> from <VAR>: <path>".
OVERRIDES = (
    ("WASM_BINDGEN_BIN", "Wasm Bindgen"),
    ("ESBUILD_BIN", "Esbuild"),
    ("WASM_OPT_BIN", "Wasm Opt"),
)


# Environment that could silently change the compiler, std or codegen of a build.
RUST_ENV_EXACT = {"RUSTFLAGS", "RUSTDOCFLAGS", "RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER",
                  "RUSTC_BOOTSTRAP", "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS",
                  "CARGO_BUILD_TARGET", "CARGO_BUILD_RUSTFLAGS", "CARGO_BUILD_RUSTC",
                  "CARGO_BUILD_RUSTC_WRAPPER", "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
                  "CARGO_BUILD_RUSTDOCFLAGS", "CARGO_BUILD_TARGET_DIR", "CARGO_TARGET_DIR",
                  "CARGO_UNSTABLE_BUILD_STD", "CARGO", "RUSTUP_HOME_OVERRIDE"}
RUST_ENV_PREFIXES = ("CARGO_TARGET_", "CARGO_PROFILE_", "CARGO_UNSTABLE_", "CARGO_BUILD_")
RUST_ENV_ALLOWED = {"CARGO_BUILD_JOBS"}


def load_pins(path: Path = MANIFEST) -> dict:
    return json.loads(path.read_text())


def download(url: str, limit: int) -> bytes:
    if not url.startswith("https://"):
        raise ValueError("tool downloads must use https")
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("tool input exceeds size limit")
    return data


def verified(data: bytes, expected: str) -> bytes:
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError("tool checksum mismatch")
    return data


def verified_integrity(data: bytes, integrity: str) -> bytes:
    algorithm, _, digest = integrity.partition("-")
    if algorithm != "sha512":
        raise ValueError("unsupported integrity algorithm")
    if base64.b64encode(hashlib.sha512(data).digest()).decode() != digest:
        raise ValueError("tool integrity mismatch")
    return data


def require_actions(env=os.environ, system: str | None = None, machine: str | None = None) -> None:
    system = sys.platform if system is None else system
    machine = platform.machine() if machine is None else machine
    if env.get("GITHUB_ACTIONS") != "true" or not system.startswith("linux") or machine != "x86_64":
        raise RuntimeError("worker tool installation is GitHub Actions Linux x86_64 only")
    for key in env:
        if key.upper().startswith(CREDENTIAL_PREFIXES):
            raise RuntimeError("worker tool installation refuses Cloudflare credentials in the environment")
    for key in ("RUNNER_TEMP", "GITHUB_ENV", "GITHUB_PATH"):
        if not env.get(key):
            raise RuntimeError(f"{key} is required")


def check_locks(pins: dict, root: Path = ROOT) -> None:
    if set(pins.get("locks", {})) != {"workers/api/Cargo.lock"}:
        raise ValueError("missing or unexpected pinned lock identities")
    for lock, digest in pins["locks"].items():
        if hashlib.sha256((root / lock).read_bytes()).hexdigest() != digest:
            raise ValueError(f"{lock} differs from the pinned lock identity")
    for worker in LOCKED_WORKERS:
        packages = tomllib.loads((root / f"workers/{worker}/Cargo.lock").read_text())["package"]
        for name, version in (("worker", pins["worker-build"]["version"]),
                              ("wasm-bindgen", pins["wasm-bindgen-cli"]["version"])):
            if {p["version"] for p in packages if p["name"] == name} != {version}:
                raise ValueError(f"{worker} locked library disagrees with {name} tool")


def install_crate(name: str, pin: dict, dl, root: Path, tool_root: Path, run=subprocess.run) -> None:
    version = pin["version"]
    source_dir = root / name
    source_dir.mkdir()
    index = dl(f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}", 4_000_000)
    entries = [json.loads(line) for line in index.splitlines() if line.strip()]
    entry = next((e for e in entries if e["vers"] == version), None)
    if entry is None or entry["yanked"] or entry["cksum"] != pin["sha256"]:
        raise ValueError("crate index disagrees with selected tool")
    archive = verified(dl(
        f"https://static.crates.io/crates/{name}/{name}-{version}.crate", 20_000_000
    ), pin["sha256"])
    (source_dir / "source.crate").write_bytes(archive)
    source = unpack_crate(archive, source_dir, f"{name}-{version}")
    package = tomllib.loads((source / "Cargo.toml").read_text())["package"]
    if package.get("name") != name or package.get("version") != version:
        raise ValueError("crate manifest disagrees with selected tool")
    if not (source / "Cargo.lock").is_file():
        raise ValueError("selected tool must ship Cargo.lock")
    # Install the verified source, keeping the process cwd outside downloaded files.
    run(["cargo", "install", "--path", str(source), "--locked",
         "--root", str(tool_root)], cwd=ROOT, check=True)


def unpack_crate(archive: bytes, destination: Path, directory: str) -> Path:
    """Write bounded regular files below the expected crate root; never follow links."""
    seen = set()
    total = 0
    with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
        for count, member in enumerate(bundle, 1):
            parts = member.name.rstrip("/").split("/")
            if (count > 10_000 or parts[0] != directory
                    or any(part in ("", ".", "..") or "\\" in part for part in parts)
                    or member.name in seen or not (member.isdir() or member.isfile())):
                raise ValueError("unexpected crate archive member")
            seen.add(member.name)
            target = destination.joinpath(*parts)
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            total += member.size
            if member.size < 0 or member.size > 20_000_000 or total > 100_000_000:
                raise ValueError("crate contents exceed size limit")
            handle = bundle.extractfile(member)
            if handle is None:
                raise ValueError("unexpected crate archive member")
            target.parent.mkdir(parents=True, exist_ok=True)
            with target.open("xb") as output:
                output.write(handle.read())
            target.chmod(0o755 if member.mode & 0o111 else 0o644)
    return destination / directory


def read_member(archive: bytes, member_name: str, limit: int) -> bytes:
    """Return the content of exactly one named regular file; never extract paths."""
    with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
        member = bundle.getmember(member_name)
        if not member.isfile() or member.size > limit:
            raise ValueError("unexpected archive member")
        handle = bundle.extractfile(member)
        if handle is None:
            raise ValueError("unexpected archive member")
        return handle.read()


def install_wasm_opt(pins: dict, dl, root: Path, tool_root: Path) -> Path:
    binaryen = pins["binaryen"]
    source_dir = root / "binaryen"
    source_dir.mkdir()
    url = ("https://github.com/WebAssembly/binaryen/releases/download/"
           f"{binaryen['version']}/{binaryen['asset']}")
    published = dl(url + ".sha256", 4096).decode().split()
    if published != [binaryen["sha256"], binaryen["asset"]]:
        raise ValueError("published binaryen checksum disagrees with selection")
    archive = verified(dl(url, 200_000_000), binaryen["sha256"])
    (source_dir / "source.tar.gz").write_bytes(archive)
    data = read_member(archive, f"binaryen-{binaryen['version']}/bin/wasm-opt", 200_000_000)
    target = tool_root / "bin/wasm-opt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    target.chmod(0o755)
    return target


def install_esbuild(pins: dict, dl, root: Path, tool_root: Path) -> Path:
    """Standalone npm linux-x64 tarball: no package manager, scripts or install step."""
    version = pins["runtime"]["esbuild"]
    package = pins["esbuild-npm"]["package"]
    leaf = package.split("/")[-1]
    source_dir = root / "esbuild"
    source_dir.mkdir()
    url = f"https://registry.npmjs.org/{package}/-/{leaf}-{version}.tgz"
    archive = verified_integrity(dl(url, 60_000_000), pins["esbuild-npm"]["integrity"])
    (source_dir / "source.tgz").write_bytes(archive)
    data = read_member(archive, "package/bin/esbuild", 60_000_000)
    target = tool_root / "bin/esbuild"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(data)
    target.chmod(0o755)
    return target


def version_matches(output: str, expected: str) -> bool:
    return re.search(rf"(?<![\d.]){re.escape(expected)}(?![\d.])", output) is not None


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


def verify_rust_toolchain(pins: dict, env=os.environ, run=subprocess.run, std_archive: bytes | None = None) -> None:
    """Fail closed unless the active toolchain is exactly the pinned tuple.

    With std_archive, the installed wasm std files are byte-compared to the verified archive."""
    rust = pins["rust"]
    rust_env_refusals(env, rust["version"])

    def out(*cmd):
        return run(list(cmd), check=True, capture_output=True, text=True, cwd=ROOT).stdout

    rustc = parse_vv(out("rustc", "-vV"))
    cargo = parse_vv(out("cargo", "-vV"))
    if (rustc.get("release") != rust["version"] or rustc.get("commit-hash") != rust["commit"]
            or rustc.get("host") != rust["host"]):
        raise ValueError("active rustc is not the pinned release, commit and host")
    if cargo.get("host") != rust["host"] or not cargo.get("commit-hash", "").startswith(rust["cargo-commit-prefix"]):
        raise ValueError("active cargo is not the pinned cargo")
    sysroot = Path(out("rustc", "--print", "sysroot").strip())
    if sysroot.name != f"{rust['version']}-{rust['host']}":
        raise ValueError("active sysroot is not the pinned toolchain")
    libdir = sysroot / "lib/rustlib" / rust["target"] / "lib"
    if not libdir.is_dir():
        raise ValueError("pinned wasm32 std is not installed")
    if std_archive is not None:
        want = tar_files(std_archive, f"rust-std-{rust['version']}-{rust['target']}/rust-std-{rust['target']}/lib/rustlib/{rust['target']}/lib/", 200_000_000)
        have = {str(f.relative_to(libdir)): hashlib.sha256(f.read_bytes()).hexdigest()
                for f in libdir.rglob("*") if f.is_file()}
        if not want or want != have:
            raise ValueError("installed wasm32 std differs from the verified official archive")


def install_rust(env=os.environ, dl=download, run=subprocess.run) -> None:
    require_actions(env)
    pins = load_pins()
    rust = pins["rust"]
    std_archive = verify_rust_source(pins, dl)
    run(["rustup", "toolchain", "install", rust["version"], "--profile", "minimal",
         "--no-self-update", "--target", rust["target"]], check=True, cwd=ROOT)
    run(["rustup", "default", rust["version"]], check=True, cwd=ROOT)
    with open(env["GITHUB_ENV"], "a") as output:
        output.write(f"RUSTUP_TOOLCHAIN={rust['version']}\n")
    verify_rust_toolchain(pins, {**env, "RUSTUP_TOOLCHAIN": rust["version"]}, run, std_archive)
    print(f"verified rust {rust['version']} {rust['commit']} {rust['host']} + {rust['target']}", flush=True)


def provenance(log_path: str, pins: dict, root: Path = ROOT) -> dict:
    """Record the verified tuple with digests of the lock and built artifacts."""
    match = re.fullmatch(r"worker-([a-z]+)\.log", Path(log_path).name)
    if not match or match.group(1) not in LOCKED_WORKERS:
        raise ValueError("build log must be named worker-<api|mb|ingest>.log")
    worker = match.group(1)
    build = root / "workers" / worker / "build"
    wasm = build / "index_bg.wasm"
    if not wasm.is_file():
        raise ValueError("build did not produce index_bg.wasm")
    lock = root / "workers" / worker / "Cargo.lock"
    rust = pins["rust"]
    return {"worker": worker, "rust": rust["version"], "commit": rust["commit"],
            "host": rust["host"], "target": rust["target"],
            "channel-manifest-sha256": rust["channel-manifest"]["sha256"],
            "cargo-lock-sha256": hashlib.sha256(lock.read_bytes()).hexdigest(),
            "worker-build": pins["worker-build"]["version"],
            "wasm-bindgen": pins["wasm-bindgen-cli"]["version"],
            "index_bg.wasm-sha256": hashlib.sha256(wasm.read_bytes()).hexdigest()}


def install(env=os.environ, dl=download, run=subprocess.run) -> None:
    require_actions(env)
    pins = load_pins()
    check_locks(pins)
    verify_rust_toolchain(pins, env, run)
    root = Path(env["RUNNER_TEMP"]) / "worker-tools"
    root.mkdir()  # A reused or nonempty root is not admissible.
    tool_root = root / "installed"
    for name in ("worker-build", "wasm-bindgen-cli"):
        install_crate(name, pins[name], dl, root, tool_root, run)
        print(f"verified {name} {pins[name]['version']} crate sha256", flush=True)
    wasm_opt = install_wasm_opt(pins, dl, root, tool_root)
    esbuild = install_esbuild(pins, dl, root, tool_root)
    paths = {
        "WASM_BINDGEN_BIN": tool_root / "bin/wasm-bindgen",
        "ESBUILD_BIN": esbuild,
        "WASM_OPT_BIN": wasm_opt,
    }
    expected = {
        "WASM_BINDGEN_BIN": pins["wasm-bindgen-cli"]["version"],
        "ESBUILD_BIN": pins["runtime"]["esbuild"],
        "WASM_OPT_BIN": pins["binaryen"]["version"].removeprefix("version_"),
    }
    for name, path in paths.items():
        if not path.is_absolute() or not os.access(path, os.X_OK):
            raise ValueError(f"{name} is not an absolute executable")
        result = run([str(path), "--version"], check=True, capture_output=True, text=True)
        print(f"{name}: {result.stdout.strip()}", flush=True)
        if not version_matches(result.stdout, expected[name]):
            raise ValueError("tool executable version disagrees with selection")
    built = run([str(tool_root / "bin/worker-build"), "--version"],
                check=True, capture_output=True, text=True)
    if not version_matches(built.stdout, pins["worker-build"]["version"]):
        raise ValueError("worker-build version disagrees with selection")
    cache = root / "empty-cache"
    cache.mkdir()
    with open(env["GITHUB_ENV"], "a") as output:
        for name, path in paths.items():
            output.write(f"{name}={path}\n")
        output.write(f"XDG_CACHE_HOME={cache}\n")
    with open(env["GITHUB_PATH"], "a") as output:
        output.write(str(tool_root / "bin") + "\n")


def check_build(log_path: str, env=os.environ) -> None:
    cache_home = env.get("XDG_CACHE_HOME")
    if not cache_home:
        raise ValueError("XDG_CACHE_HOME is not set to the prepared cache")
    cache = Path(cache_home) / "worker-build"
    if cache.exists() and any(cache.iterdir()):
        raise ValueError("worker-build downloaded or cached an unverified tool")
    # The styled prefix is "[INFO]"; strip any ANSI colour and CR so a coloured or
    # CRLF-captured log still matches. Paths are printed raw, unquoted.
    text = ANSI.sub("", Path(log_path).read_text(errors="replace")).replace("\r", "")
    tool_download = any(f"Downloading {name}@" in text for _, name in OVERRIDES)
    if "falling back to internal" in text or tool_download:
        raise ValueError("worker-build fell back or attempted a tool download")
    for var, full_name in OVERRIDES:
        path = env.get(var)
        if not path or not os.path.isabs(path):
            raise ValueError(f"{var} is not an absolute path")
        line = re.compile(rf"Using custom {re.escape(full_name)} from {var}: (.+)$", re.M)
        used = {m.group(1).strip() for m in line.finditer(text)}
        if used != {path}:
            raise ValueError(f"missing or mismatched verified override {var}")
    print("worker-build used all three verified overrides; download cache empty")


def main(argv: list[str]) -> int:
    if argv[1:2] == ["rust"] and len(argv) == 2:
        install_rust()
    elif argv[1:2] == ["install"] and len(argv) == 2:
        install()
    elif argv[1:2] == ["check-build"] and len(argv) == 3:
        pins = load_pins()
        check_locks(pins)
        verify_rust_toolchain(pins)
        check_build(argv[2])
        record = provenance(argv[2], pins)
        print("worker build provenance: " + json.dumps(record, sort_keys=True), flush=True)
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
