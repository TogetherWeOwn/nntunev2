#!/usr/bin/env python3
"""Validate Subsonic golden fixtures without the Rust toolchain.

Checks every file in fixtures/subsonic/: XML prolog + envelope shape, JSON
parse + `subsonic-response` root, the routes manifest counts (48 supported,
9 unsupported), and that the README documents the contract. The byte-exact
comparison against handler output lives in the Rust fixture suite; this is
the cheap CI gate that catches malformed or misnamed fixtures.
"""

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIX = ROOT / "fixtures" / "subsonic"

failures = []


def fail(msg):
    failures.append(msg)
    print(f"FAIL: {msg}")


def main():
    if not FIX.is_dir():
        fail(f"missing directory {FIX}")
        return 1

    names = sorted(p.name for p in FIX.iterdir() if p.is_file() and p.name != "README.md")
    if not names:
        fail("no fixtures found")
        return 1

    manifests = {"routes-table.json"}
    for name in names:
        if name in manifests:
            continue  # validated separately below
        path = FIX / name
        raw = path.read_bytes()
        if name.endswith(".xml"):
            text = raw.decode("utf-8")
            if not text.startswith('<?xml version="1.0" encoding="UTF-8"?>'):
                fail(f"{name}: missing XML prolog")
            if "<subsonic-response " not in text:
                fail(f"{name}: missing envelope root")
            stripped = text.rstrip()
            if not (
                stripped.endswith("</subsonic-response>") or stripped.endswith("/>")
            ):
                fail(f"{name}: envelope not closed")
            if b"\n" in raw.strip():
                fail(f"{name}: must be a single line (no pretty-printing)")
        elif name.endswith(".json"):
            try:
                doc = json.loads(raw.decode("utf-8"))
            except json.JSONDecodeError as e:
                fail(f"{name}: invalid JSON: {e}")
                continue
            if "subsonic-response" not in doc:
                fail(f"{name}: missing subsonic-response root")
            if raw.strip().endswith(b"\n") or b"\n" in raw.strip():
                fail(f"{name}: must be compact single-line JSON")
        elif name.endswith(".txt"):
            if b"subsonic-response" not in raw:
                fail(f"{name}: missing envelope")
        else:
            fail(f"{name}: unexpected extension (want .xml/.json/.txt)")

    manifest = FIX / "routes-table.json"
    if manifest.exists():
        doc = json.loads(manifest.read_text())
        if len(doc.get("supported", [])) != 48:
            fail(f"routes-table.json: want 48 supported, got {len(doc.get('supported', []))}")
        if len(doc.get("unsupported", [])) != 9:
            fail(f"routes-table.json: want 9 unsupported, got {len(doc.get('unsupported', []))}")
    else:
        fail("routes-table.json missing")

    for golden in ("ping.xml", "ping.json", "error-auth-required.xml"):
        if golden not in names:
            fail(f"missing required fixture {golden}")

    if not (FIX / "README.md").exists():
        fail("fixtures README.md missing")

    print(f"checked {len(names)} fixtures: {'OK' if not failures else f'{len(failures)} failures'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
