#!/usr/bin/env python3
"""Inspect checksum-pinned Worker archives without extracting or executing them.

Diagnostic output is not a compatibility or publisher-identity attestation.
"""

import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
CHECKSUMS = {
    "0.8.7": "4057bce8ec66c80b7e270d884c312e2a4ff60eb997fd02936656106c06a964e5",
    "0.9.0": "1d055db1ea6baa6c329730897f63ff7b6ac93c46a9f053804e497e5960eb98e3",
}
LOCKS = {
    "Cargo.lock": "0.8.7",
    "workers/repair/Cargo.lock": "0.8.7",
    "workers/api/Cargo.lock": "0.9.0",
    "workers/ingest/Cargo.lock": "0.9.0",
    "workers/mb/Cargo.lock": "0.9.0",
    "workers/transcode/Cargo.lock": "0.9.0",
}
MAX_ARCHIVE = 10_000_000
MAX_TOTAL = 30_000_000
PATTERN = re.compile(r"\bchrono\b|\btimezone\b", re.IGNORECASE)


def verify_locks(root):
    for name, version in LOCKS.items():
        data = (root / name).read_bytes()
        packages = tomllib.loads(data.decode("utf-8")).get("package", [])
        workers = [p for p in packages if p.get("name") == "worker"]
        expected = {"version": version, "checksum": CHECKSUMS[version],
                    "source": "registry+https://github.com/rust-lang/crates.io-index"}
        if len(workers) != 1 or any(workers[0].get(k) != v for k, v in expected.items()):
            raise ValueError(f"unexpected Worker identity in {name}")
        print(f"input {name} sha256={hashlib.sha256(data).hexdigest()} worker={version}")


def inspect_archive(data, version):
    if version not in CHECKSUMS or len(data) > MAX_ARCHIVE:
        raise ValueError("unexpected archive version or size")
    if hashlib.sha256(data).hexdigest() != CHECKSUMS[version]:
        raise ValueError("archive checksum mismatch")
    prefix = f"worker-{version}/"
    seen = set()
    total = 0
    matches = []
    durable = None
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        for member in archive:
            if not member.isfile() or not member.name.startswith(prefix):
                raise ValueError("unexpected archive entry")
            name = member.name[len(prefix):]
            path = PurePosixPath(name)
            if not name or path.is_absolute() or ".." in path.parts or str(path) != name or name in seen:
                raise ValueError("unsafe or duplicate archive path")
            seen.add(name)
            total += member.size
            if len(seen) > 10000 or member.size > MAX_ARCHIVE or total > MAX_TOTAL:
                raise ValueError("archive exceeds inspection bounds")
            if name != "Cargo.toml" and not (name.startswith("src/") and name.endswith(".rs")):
                continue
            source = archive.extractfile(member)
            if source is None:
                raise ValueError("missing archive member")
            content = source.read().decode("utf-8").splitlines()
            if version == "0.9.0" and name == "src/durable.rs":
                if len(content) > 1500 or sum(map(len, content)) > 200_000 or any(len(line) > 1000 for line in content):
                    raise ValueError("durable source exceeds output bounds")
                durable = content
            for number, line in enumerate(content, 1):
                if PATTERN.search(line):
                    if len(line) > 1000 or len(matches) >= 500:
                        raise ValueError("diagnostic output exceeds bounds")
                    matches.append({"version": version, "file": name, "line": number, "text": line})
    if not seen or not matches:
        raise ValueError("archive has no files or compatibility references")
    if version == "0.9.0":
        if durable is None or not any("ScheduledTime" in line for line in durable):
            raise ValueError("missing ScheduledTime source")
        print(f"pristine worker={version} sha256={CHECKSUMS[version]} file=src/durable.rs")
        for number, line in enumerate(durable, 1):
            print("source " + json.dumps({"line": number, "text": line}, ensure_ascii=True))
    return matches


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise ValueError("archive redirect refused")


def main():
    if os.environ.get("GITHUB_ACTIONS") != "true":
        raise RuntimeError("only run on a GitHub Actions runner")
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT, check=True,
                            capture_output=True, text=True).stdout.strip()
    if commit != os.environ.get("EXPECTED_HEAD_SHA"):
        raise RuntimeError("checked-out commit does not match expected PR head")
    print(f"input commit={commit}")
    verify_locks(ROOT)
    opener = urllib.request.build_opener(NoRedirect())
    for version in CHECKSUMS:
        url = f"https://static.crates.io/crates/worker/worker-{version}.crate"
        with opener.open(url, timeout=30) as response:
            data = response.read(MAX_ARCHIVE + 1)
        for match in inspect_archive(data, version):
            print("compatibility " + json.dumps(match, ensure_ascii=True))
    print("Diagnostic only: no sources extracted, patched, or published.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, RuntimeError, ValueError, tarfile.TarError, subprocess.CalledProcessError) as error:
        print(f"worker archive inspection failed: {error}", file=sys.stderr)
        sys.exit(1)
