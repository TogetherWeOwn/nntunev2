#!/usr/bin/env python3
"""Fail the dependency gate when the temporary advisory exception expires."""

import datetime as dt
import sys
import tomllib
from pathlib import Path

ADVISORY = "RUSTSEC-2026-0543"
# Conservative boundary at the start of the recorded review date, UTC.
EXPIRY = dt.datetime(2027, 2, 17, tzinfo=dt.timezone.utc)


def check(policy: dict, now: dt.datetime) -> None:
    ignores = policy["advisories"].get("ignore", [])
    if not isinstance(ignores, list):
        raise ValueError("advisory ignore must be a list")
    ids = []
    for entry in ignores:
        if isinstance(entry, str):
            ids.append(entry)
        elif isinstance(entry, dict) and isinstance(entry.get("id"), str):
            ids.append(entry["id"])
        else:
            raise ValueError("invalid advisory ignore entry")
    if ADVISORY in ids and now >= EXPIRY:
        raise ValueError(f"{ADVISORY} exception expired at {EXPIRY:%Y-%m-%dT%H:%M:%SZ}")


def main() -> int:
    try:
        with Path("deny.toml").open("rb") as policy_file:
            policy = tomllib.load(policy_file)
        check(policy, dt.datetime.now(dt.timezone.utc))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"advisory expiry check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
