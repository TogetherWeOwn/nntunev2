"""Install qualification-only tools in a fresh Actions temp directory, never on the host."""

import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
PINS = json.loads((ROOT / ".github/worker-toolchain.json").read_text())


def download(url, limit):
    with urllib.request.urlopen(url, timeout=120) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError("tool input exceeds size limit")
    return data


def verified(data, expected):
    if hashlib.sha256(data).hexdigest() != expected:
        raise ValueError("tool checksum mismatch")
    return data


def check_pins():
    runtime = PINS["runtime"]
    deploy = (ROOT / ".github/workflows/deploy.yml").read_text()
    selected = re.search(r"^  WRANGLER_VERSION: (\S+)$", deploy, re.M).group(1)
    if selected != runtime["wrangler"]:
        raise ValueError("qualification runtime is stale against deploy Wrangler")
    harness = ROOT / "workers/api/qualification"
    package = json.loads((harness / "package.json").read_text())
    if package["dependencies"] != {
        "miniflare": runtime["miniflare"], "@esbuild/linux-x64": runtime["esbuild"],
    }:
        raise ValueError("qualification importer disagrees with canonical pins")
    lock = (harness / "pnpm-lock.yaml").read_text()
    for name, version in [("miniflare", runtime["miniflare"]), ("workerd", runtime["workerd"]),
                          ("@esbuild/linux-x64", runtime["esbuild"])]:
        found = set(re.findall(rf"^  '?{re.escape(name)}@([^:' ]+)'?:", lock, re.M))
        if found != {version}:
            raise ValueError(f"qualification lock disagrees for {name}")
    for worker in ("api", "ingest"):
        packages = tomllib.loads((ROOT / f"workers/{worker}/Cargo.lock").read_text())["package"]
        for name, version in (("worker", PINS["worker-build"]["version"]),
                              ("wasm-bindgen", PINS["wasm-bindgen-cli"]["version"])):
            if {p["version"] for p in packages if p["name"] == name} != {version}:
                raise ValueError(f"{worker} locked library disagrees with {name} tool")
    ci = (ROOT / ".github/workflows/ci.yml").read_text()
    pnpm = re.search(r"^  PNPM_VERSION: (\S+)$", ci, re.M).group(1)
    if package["packageManager"] != "pnpm@" + pnpm:
        raise ValueError("qualification must reuse CI's package-manager version")
    policy = (harness / "pnpm-workspace.yaml").read_text()
    if not re.search(r"^minimumReleaseAge: 4320$", policy, re.M) or not re.search(
        r"^ignoreScripts: true$", policy, re.M
    ):
        raise ValueError("qualification package-manager controls missing")
    if list(harness.glob(".pnpmfile.*")):
        raise ValueError("qualification must not load a package-manager hook")
    # Inspect published metadata only; never install or import Wrangler here.
    wrangler = json.loads(download(f"https://registry.npmjs.org/wrangler/{selected}", 1_000_000))
    miniflare = json.loads(download(
        f"https://registry.npmjs.org/miniflare/{runtime['miniflare']}", 1_000_000
    ))
    for name in ("miniflare", "workerd", "esbuild"):
        declared = wrangler.get("dependencies", {}).get(name)
        if name == "workerd" and declared is None:
            declared = miniflare["dependencies"][name]
        if declared != runtime[name]:
            raise ValueError(f"deploy Wrangler runtime disagrees for {name}")
    print("canonical manifest, isolated lock and deploy Wrangler runtime agree")


def install():
    if os.environ.get("GITHUB_ACTIONS") != "true" or sys.platform != "linux":
        raise RuntimeError("qualification tool installation is Actions-only")
    root = Path(os.environ["RUNNER_TEMP"]) / "preparation-tools"
    root.mkdir()  # A reused/nonempty download or install root is not admissible.
    tool_root = root / "installed"
    for name in ("worker-build", "wasm-bindgen-cli"):
        pin = PINS[name]
        source_dir = root / name
        source_dir.mkdir()
        version = pin["version"]
        index = download(f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}", 4_000_000)
        entries = [json.loads(line) for line in index.splitlines()]
        entry = next(e for e in entries if e["vers"] == version)
        if entry["yanked"] or entry["cksum"] != pin["sha256"]:
            raise ValueError("crate index disagrees with selected tool")
        archive = verified(download(
            f"https://static.crates.io/crates/{name}/{name}-{version}.crate", 20_000_000
        ), pin["sha256"])
        (source_dir / "source.crate").write_bytes(archive)
        # Install the checksum-verified archive itself, not a second registry
        # fetch of the same version. The crate is unpacked with the data filter
        # (no links, absolute paths or escapes) and built with its shipped lock.
        # Only locked dependency downloads still use the network. Never execute
        # from the downloaded source directory.
        unpack_dir = source_dir / "src"
        unpack_dir.mkdir()
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as crate:
            crate.extractall(unpack_dir, filter="data")
        package_dir = unpack_dir / f"{name}-{version}"
        if not (package_dir / "Cargo.toml").is_file() or not (package_dir / "Cargo.lock").is_file():
            raise ValueError("verified crate lacks manifest or lockfile")
        subprocess.run([
            "cargo", "install", "--path", str(package_dir), "--locked",
            "--root", str(tool_root),
        ], cwd=ROOT, check=True)
        print(f"verified {name} {version} crate sha256", flush=True)

    binaryen = PINS["binaryen"]
    source_dir = root / "binaryen"
    source_dir.mkdir()
    base = "https://github.com/WebAssembly/binaryen/releases/download/"
    url = f"{base}{binaryen['version']}/{binaryen['asset']}"
    published = download(url + ".sha256", 4096).decode().split()
    if published != [binaryen["sha256"], binaryen["asset"]]:
        raise ValueError("published binaryen checksum disagrees with selection")
    archive = verified(download(url, 200_000_000), binaryen["sha256"])
    (source_dir / "source.tar.gz").write_bytes(archive)
    # Extract exactly one regular executable, never archive paths or links.
    member_name = f"binaryen-{binaryen['version']}/bin/wasm-opt"
    with tarfile.open(fileobj=io.BytesIO(archive)) as bundle:
        member = bundle.getmember(member_name)
        if not member.isfile() or member.size > 200_000_000:
            raise ValueError("unexpected wasm-opt archive member")
        executable = bundle.extractfile(member).read()
    wasm_opt = tool_root / "bin/wasm-opt"
    wasm_opt.write_bytes(executable)
    wasm_opt.chmod(0o755)
    esbuild = ROOT / "workers/api/qualification/node_modules/@esbuild/linux-x64/bin/esbuild"
    if not esbuild.is_file() or not os.access(esbuild, os.X_OK):
        raise ValueError("integrity-locked esbuild executable missing")
    paths = {
        "WASM_BINDGEN_BIN": tool_root / "bin/wasm-bindgen",
        "ESBUILD_BIN": esbuild,
        "WASM_OPT_BIN": wasm_opt,
    }
    versions = {
        "WASM_BINDGEN_BIN": PINS["wasm-bindgen-cli"]["version"],
        "ESBUILD_BIN": PINS["runtime"]["esbuild"],
        "WASM_OPT_BIN": binaryen["version"].removeprefix("version_"),
    }
    for name, path in paths.items():
        result = subprocess.run([str(path), "--version"], check=True, capture_output=True, text=True)
        print(f"{name}: {result.stdout.strip()}", flush=True)
        if not re.search(rf"(?<![\d.]){re.escape(versions[name])}(?![\d.])", result.stdout):
            raise ValueError("tool executable version disagrees with selection")
        if not os.access(path, os.X_OK):
            raise ValueError("tool override is not executable")
    subprocess.run([str(tool_root / "bin/worker-build"), "--version"], check=True)
    rustc = subprocess.check_output(["rustc", "-V"], text=True).strip()
    print(rustc, flush=True)
    match = re.match(r"rustc (\d+)\.(\d+)\.", rustc)
    if not match or tuple(map(int, match.groups())) < (1, 91):
        raise ValueError("qualification requires Rust >= 1.91")
    with open(os.environ["GITHUB_ENV"], "a") as output:
        for name, path in paths.items():
            output.write(f"{name}={path}\n")
        output.write(f"XDG_CACHE_HOME={root / 'empty-cache'}\n")
    with open(os.environ["GITHUB_PATH"], "a") as output:
        output.write(str(tool_root / "bin") + "\n")


def check_builds():
    cache = Path(os.environ["XDG_CACHE_HOME"]) / "worker-build"
    if cache.exists() and any(cache.iterdir()):
        raise ValueError("worker-build downloaded or cached an unverified tool")
    for name in ("api", "ingest", "composite"):
        text = (Path(os.environ["RUNNER_TEMP"]) / f"qualification-{name}.log").read_text()
        for override in ("WASM_BINDGEN_BIN", "ESBUILD_BIN", "WASM_OPT_BIN"):
            if not re.search(rf"Using custom .* from {override}:", text):
                raise ValueError(f"{name}: missing verified override {override}")
        if "falling back to internal" in text or "Downloading " in text:
            raise ValueError("worker-build attempted a tool download")
    print("all three release builds used all three overrides; download cache empty")


if __name__ == "__main__":
    {"check-pins": check_pins, "install": install, "check-builds": check_builds}[sys.argv[1]]()
