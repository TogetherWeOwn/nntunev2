#!/usr/bin/env python3
"""Validate Subsonic golden fixtures without the Rust toolchain.

Checks XML, JSON and JSONP response envelopes, the route manifest (49 supported,
9 unsupported), and required files in fixtures/subsonic/. The Rust fixture
suite compares the golden bytes against handler output.
"""

import json
import re
import sys
from pathlib import Path
from xml.etree import ElementTree

ROOT = Path(__file__).resolve().parent.parent
FIX = ROOT / "fixtures" / "subsonic"
ENVELOPE = "subsonic-response"
PROLOG = '<?xml version="1.0" encoding="UTF-8"?>'
ROUTE_COUNTS = {"supported": 49, "unsupported": 9}


def unique_object(pairs):
    fields = dict(pairs)
    if len(fields) != len(pairs):
        raise ValueError("duplicate JSON fields")
    return fields


def reject_constant(_value):
    raise ValueError("non-finite JSON constant")


def parse_json(text):
    return json.loads(text, object_pairs_hook=unique_object, parse_constant=reject_constant)


def valid_routes(doc):
    if not isinstance(doc, dict) or doc.keys() != ROUTE_COUNTS.keys():
        return False
    seen = set()
    for kind, count in ROUTE_COUNTS.items():
        routes = doc[kind]
        if not isinstance(routes, list) or len(routes) != count:
            return False
        for route in routes:
            if (not isinstance(route, str) or not re.fullmatch(r"[a-zA-Z][a-zA-Z0-9]*", route)
                    or route in seen):
                return False
            seen.add(route)
    return True


def valid_envelope(doc):
    return (isinstance(doc, dict) and doc.get("status") in ("ok", "failed")
            and doc.get("version") == "1.16.1")


def valid_fixture(path):
    text = path.read_bytes().decode("utf-8")
    if path.name == "routes-table.json":
        return valid_routes(parse_json(text))
    if "\r" in text or "\n" in text:
        return False
    if path.suffix == ".xml":
        if not text.startswith(PROLOG) or "<!DOCTYPE" in text:
            return False
        if not text.rstrip().endswith((f"</{ENVELOPE}>", "/>")):
            return False
        root = ElementTree.fromstring(text)
        return root.tag == ENVELOPE and valid_envelope(root.attrib)
    if path.name == "ping-jsonp.txt":
        # The byte-exact Rust golden test requests the literal callback cb.
        if not text.startswith("/**/cb(") or not text.endswith(");"):
            return False
        text = text.removeprefix("/**/cb(").removesuffix(");")
    elif path.suffix != ".json":
        return False
    doc = parse_json(text)
    return isinstance(doc, dict) and doc.keys() == {ENVELOPE} and valid_envelope(doc[ENVELOPE])


def main():
    try:
        # Resolve before listing so a symlink anywhere in the fixed path is refused.
        if FIX.resolve(strict=True) != FIX:
            raise ValueError("linked fixture directory")
        paths = sorted(FIX.iterdir())
    except (OSError, RuntimeError, ValueError):
        print("FAIL: missing, linked or unreadable fixture directory")
        return 1

    failures = []
    names = set()
    for path in paths:
        try:
            if path.is_symlink():
                failures.append(f"{path.name!r}: linked fixture")
            elif path.is_file():
                names.add(path.name)
                if path.name != "README.md" and not valid_fixture(path):
                    failures.append(f"{path.name!r}: invalid fixture")
            elif not path.is_dir():
                failures.append(f"{path.name!r}: expected regular fixture file")
        except (OSError, ValueError, RecursionError, ElementTree.ParseError):
            failures.append(f"{path.name!r}: unreadable or malformed fixture")

    for name in ("README.md", "routes-table.json", "ping.xml", "ping.json",
                 "ping-jsonp.txt", "error-auth-required.xml"):
        if name not in names:
            failures.append(f"missing required fixture {name}")

    for failure in failures:
        print(f"FAIL: {failure}")

    count = len(names - {"README.md"})
    print(f"checked {count} fixtures: {'OK' if not failures else f'{len(failures)} failures'}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
