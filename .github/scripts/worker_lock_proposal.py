"""Generate a review-only Worker lock proposal on the pinned PR head in Actions."""

import difflib
import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKERS = ("api", "mb", "ingest")
TOOLS = {"worker-build": "0.9.0", "wasm-bindgen-cli": "0.2.130"}
TOOLCHAIN = "1.98.1"


def run(*args, capture=False):
    return subprocess.run(
        args,
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
    ).stdout


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def download(url, limit):
    request = urllib.request.Request(
        url, headers={"User-Agent": "nntune-worker-lock-proposal"}
    )
    with urllib.request.urlopen(request, timeout=60) as response:
        data = response.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"response too large: {url}")
    return data


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("Actions only")
    head = os.environ["PR_HEAD_SHA"]
    if len(head) != 40 or any(c not in "0123456789abcdef" for c in head):
        raise ValueError("invalid PR head SHA")
    if run("git", "rev-parse", "HEAD", capture=True).strip() != head:
        raise ValueError("checkout does not match PR head")
    allowed = {f"workers/{worker}/Cargo.lock" for worker in WORKERS}
    report = [
        f"PR head: {head}",
        f"workflow revision: {head}:.github/workflows/ci.yml",
        f"run: {os.environ['GITHUB_RUN_ID']} attempt {os.environ['GITHUB_RUN_ATTEMPT']}",
        run("rustup", "run", TOOLCHAIN, "rustc", "--version", capture=True).strip(),
        run("rustup", "run", TOOLCHAIN, "cargo", "--version", capture=True).strip(),
    ]
    for worker in WORKERS:
        manifest = f"workers/{worker}/Cargo.toml"
        report.append(f"{manifest} sha256 {digest(ROOT / manifest)}")
        for package, version in (("worker", "0.9.0"), ("wasm-bindgen", "0.2.130")):
            run(
                "rustup",
                "run",
                TOOLCHAIN,
                "cargo",
                "update",
                "--manifest-path",
                manifest,
                "-p",
                package,
                "--precise",
                version,
            )
    pins = json.loads((ROOT / ".github/worker-toolchain.json").read_text())
    with tempfile.TemporaryDirectory(
        dir=os.environ["RUNNER_TEMP"], prefix="worker-proposal-"
    ) as temp:
        for name, version in TOOLS.items():
            directory = Path(temp) / name
            directory.mkdir()
            index_url = f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
            archive_url = (
                f"https://static.crates.io/crates/{name}/{name}-{version}.crate"
            )
            metadata_url = f"https://crates.io/api/v1/crates/{name}/{version}"
            index = download(index_url, 4_000_000)
            entries = [json.loads(line) for line in index.splitlines() if line.strip()]
            matches = [entry for entry in entries if entry["vers"] == version]
            if len(matches) != 1 or matches[0]["yanked"]:
                raise ValueError(
                    f"missing, duplicate or yanked release: {name} {version}"
                )
            archive = download(archive_url, 20_000_000)
            checksum = hashlib.sha256(archive).hexdigest()
            if checksum != matches[0]["cksum"]:
                raise ValueError(f"archive checksum mismatch: {name}")
            metadata = json.loads(download(metadata_url, 1_000_000))["version"]
            if metadata["num"] != version or metadata["yanked"]:
                raise ValueError(f"release metadata mismatch: {name}")
            (directory / "source.crate").write_bytes(archive)
            pins[name] = {"version": version, "sha256": checksum}
            report.append(
                f"{name} {version} sha256 {checksum} published {metadata['created_at']}\n"
                f"  index {index_url}\n  archive {archive_url}\n  metadata {metadata_url}"
            )
    spec = importlib.util.spec_from_file_location(
        "worker_tools", ROOT / ".github/scripts/worker_tools.py"
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    module.check_locks(pins)
    before = {
        worker: digest(ROOT / f"workers/{worker}/Cargo.lock") for worker in WORKERS
    }
    for worker in WORKERS:
        run(
            "rustup",
            "run",
            TOOLCHAIN,
            "cargo",
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--manifest-path",
            f"workers/{worker}/Cargo.toml",
            capture=True,
        )
    if before != {
        worker: digest(ROOT / f"workers/{worker}/Cargo.lock") for worker in WORKERS
    }:
        raise ValueError("metadata changed a lockfile")
    changed = set(run("git", "diff", "--name-only", capture=True).splitlines())
    if not changed <= allowed:
        raise ValueError(f"unexpected changed paths: {sorted(changed - allowed)}")
    output = Path(os.environ["RUNNER_TEMP"]) / "worker-lock-proposal"
    output.mkdir()
    for worker in WORKERS:
        target = output / f"workers/{worker}/Cargo.lock"
        target.parent.mkdir(parents=True)
        target.write_bytes((ROOT / f"workers/{worker}/Cargo.lock").read_bytes())
    target = output / ".github/worker-toolchain.json"
    target.parent.mkdir()
    target.write_text(json.dumps(pins, indent=2) + "\n")
    report.extend(
        difflib.unified_diff(
            (ROOT / ".github/worker-toolchain.json").read_text().splitlines(),
            target.read_text().splitlines(),
            fromfile="current toolchain",
            tofile="proposed toolchain",
        )
    )
    report.append(
        "Cargo has no stable native release-age filter or blanket build-script/proc-macro refusal."
    )
    diff = run("git", "diff", "--", *sorted(allowed), capture=True)
    if len(diff.encode()) > 2_000_000:
        raise ValueError("lock diff exceeds report bound")
    (output / "report.txt").write_text("\n".join(report) + "\n\n" + diff)


if __name__ == "__main__":
    main()
