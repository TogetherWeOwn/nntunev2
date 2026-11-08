#!/usr/bin/env python3
"""Prepare and audit pinned worker build tools (GitHub Actions on Linux x86_64 only).

Usage:
  worker_tools.py install
  worker_tools.py check-build <worker-build-log>

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
    # Cargo re-checks registry packages and uses the shipped lock. Never run
    # anything from the download directory.
    run(["cargo", "install", name, "--version", f"={version}", "--locked",
         "--root", str(tool_root)], cwd=ROOT, check=True)


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


def install(env=os.environ, dl=download, run=subprocess.run) -> None:
    require_actions(env)
    pins = load_pins()
    check_locks(pins)
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
    if argv[1:2] == ["install"] and len(argv) == 2:
        install()
    elif argv[1:2] == ["check-build"] and len(argv) == 3:
        check_build(argv[2])
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
