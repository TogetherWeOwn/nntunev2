#!/usr/bin/env python3
"""Deterministic offline artist-count coverage; no taxonomy resolution or app wiring.

Input contract and real-data limitations: docs/genre-root-coverage.md.
"""

from __future__ import annotations

import argparse
from decimal import Decimal, localcontext
from fractions import Fraction
import json
from pathlib import Path
import re
import sys

MAX_INPUT_BYTES = 32 * 1024 * 1024
MBID = re.compile(r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\Z")
ROOT = re.compile(r"[a-z][a-z0-9-]{0,63}\Z")


class InputError(ValueError):
    """Malformed input; errors describe the contract, not source values."""


def _object(value: object, keys: set[str]) -> dict:
    if not isinstance(value, dict) or set(value) != keys:
        raise InputError("object fields do not match the v1 contract")
    return value


def _array(value: object) -> list:
    if not isinstance(value, list):
        raise InputError("expected an array")
    return value


def _mbid(value: object) -> str:
    if not isinstance(value, str) or not MBID.fullmatch(value):
        raise InputError("expected a hyphenated artist MBID")
    if value == "00000000-0000-0000-0000-000000000000":
        raise InputError("nil artist MBIDs are not allowed")
    return value.lower()


def _root(value: object) -> str:
    if not isinstance(value, str) or not ROOT.fullmatch(value):
        raise InputError("root IDs must be lowercase ASCII slugs of 1-64 characters")
    return value


def calculate(document: object) -> dict:
    data = _object(document, {"schema_version", "dataset_kind", "playable_artist_mbids",
                              "resolved_root_ids", "memberships"})
    if type(data["schema_version"]) is not int or data["schema_version"] != 1:
        raise InputError("schema_version must be integer 1")
    if data["dataset_kind"] not in ("synthetic", "actual-export"):
        raise InputError("dataset_kind must be synthetic or actual-export")
    playable = {_mbid(value) for value in _array(data["playable_artist_mbids"])}
    roots = {_root(value) for value in _array(data["resolved_root_ids"])}
    covered = set()
    unknown_roots = set()
    for value in _array(data["memberships"]):
        row = _object(value, {"artist_mbid", "root_id"})
        artist = _mbid(row["artist_mbid"])
        root = None if row["root_id"] is None else _root(row["root_id"])
        if artist not in playable:
            continue
        if root in roots:
            covered.add(artist)
        elif root is not None:
            unknown_roots.add(root)
    numerator, denominator = len(covered), len(playable)
    percentage = Fraction(100 * numerator, denominator) if denominator else None
    with localcontext() as context:
        context.prec = 40
        display = (format(Decimal(percentage.numerator) / Decimal(percentage.denominator), ".6f")
                   if percentage is not None else None)
    met = denominator > 0 and numerator * 5 >= denominator * 4
    return {
        "schema_version": 1,
        "dataset_kind": data["dataset_kind"],
        "metric": "unique-playable-artist-count",
        "playable_artists": denominator,
        "artists_with_resolved_root": numerator,
        "uncovered_artists": denominator - numerator,
        "numerator": numerator,
        "denominator": denominator,
        "percentage_exact": str(percentage) if percentage is not None else None,
        "percentage_display": display,
        "threshold_percentage": 80,
        "threshold_met": met,
        "status": ("no_playable_artists" if not denominator else
                   "meets_threshold" if met else "below_threshold"),
        "unknown_root_ids_for_playable_artists": sorted(unknown_roots),
        "real_data_gate": "not_evaluated",
    }


def _unique_fields(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for key, value in pairs:
        if key in result:
            raise InputError("duplicate JSON object field")
        result[key] = value
    return result


def _invalid_constant(value: str) -> None:
    raise InputError("non-finite JSON numbers are not allowed")


def load(path: Path) -> object:
    with path.open("rb") as source:
        raw = source.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise InputError("input exceeds the 32 MiB limit; export a bounded input")
    try:
        return json.loads(raw.decode("utf-8"), object_pairs_hook=_unique_fields,
                          parse_constant=_invalid_constant)
    except (UnicodeError, json.JSONDecodeError, RecursionError, ValueError) as error:
        raise InputError("invalid UTF-8 JSON or duplicate object fields") from error


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", type=Path, help="local v1 JSON input (maximum 32 MiB)")
    args = parser.parse_args(argv)
    try:
        result = calculate(load(args.input))
    except (InputError, OSError) as error:
        # Do not echo input, paths, or OS diagnostics into reports.
        message = str(error) if isinstance(error, InputError) else "cannot read input file"
        print(f"coverage input error: {message}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True, indent=2, ensure_ascii=True))
    return 0 if result["threshold_met"] else 1


if __name__ == "__main__":
    sys.exit(main())
