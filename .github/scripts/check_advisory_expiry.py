#!/usr/bin/env python3
"""Fail the dependency gate when the temporary advisory exception expires."""

import datetime as dt
import re
import sys
import tomllib
from pathlib import Path

ADVISORY = "RUSTSEC-2026-0543"
# Conservative boundary at the start of the recorded review date, UTC.
EXPIRY = dt.datetime(2027, 2, 17, tzinfo=dt.timezone.utc)

_REVIEW_BY_RE = re.compile(r"\breview(\s+)by(?:(\s+)(\S+))?", re.IGNORECASE)
_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_UTC_TIMESTAMP_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


def _validate_now(now: dt.datetime) -> dt.datetime:
    if not isinstance(now, dt.datetime) or now.tzinfo is None or now.utcoffset() is None:
        raise ValueError("now must be a timezone-aware datetime")
    return now.astimezone(dt.timezone.utc)


def parse_review_deadline(reason: str) -> dt.datetime:
    if not isinstance(reason, str):
        raise ValueError(f"{ADVISORY} ignore reason must be a string")
    matches = list(_REVIEW_BY_RE.finditer(reason))
    if len(matches) != 1:
        raise ValueError(
            f"{ADVISORY} ignore reason must declare exactly one 'review by YYYY-MM-DD' deadline"
        )
    between, sep, raw = matches[0].groups()
    if (
        not raw
        or not sep
        or not all(ch in " \t" for ch in between)
        or not all(ch in " \t" for ch in sep)
    ):
        raise ValueError(
            f"{ADVISORY} ignore reason must declare a single-line 'review by YYYY-MM-DD' deadline"
        )
    token = re.sub(r"\)?[.,;]?$", "", raw)
    if _DATE_RE.fullmatch(token):
        parsed_date = dt.date.fromisoformat(token)
        return dt.datetime(
            parsed_date.year,
            parsed_date.month,
            parsed_date.day,
            tzinfo=dt.timezone.utc,
        )
    if _UTC_TIMESTAMP_RE.fullmatch(token):
        return dt.datetime.fromisoformat(token).astimezone(dt.timezone.utc)
    raise ValueError(f"malformed {ADVISORY} review deadline: {raw!r}")


def check(policy: dict, now: dt.datetime) -> None:
    now_utc = _validate_now(now)
    if not isinstance(policy, dict):
        raise ValueError("policy must be a mapping")
    advisories = policy["advisories"]
    if not isinstance(advisories, dict):
        raise ValueError("advisories section must be a mapping")
    ignores = advisories.get("ignore", [])
    if not isinstance(ignores, list):
        raise ValueError("advisory ignore must be a list")

    for entry in ignores:
        if isinstance(entry, str):
            entry_id = entry
            reason = None
        elif isinstance(entry, dict) and isinstance(entry.get("id"), str):
            entry_id = entry["id"]
            if "reason" in entry and not isinstance(entry["reason"], str):
                raise ValueError("advisory ignore reason must be a string")
            reason = entry.get("reason")
        else:
            raise ValueError("invalid advisory ignore entry")

        if not entry_id.strip():
            raise ValueError("invalid advisory ignore entry")

        if entry_id.strip().upper() != ADVISORY:
            continue

        if entry_id != ADVISORY:
            raise ValueError(f"{ADVISORY} ignore id must be exact canonical {ADVISORY!r}")

        if reason is None:
            raise ValueError(
                f"{ADVISORY} exception must include a reason with a review deadline on or before "
                f"{EXPIRY:%Y-%m-%dT%H:%M:%SZ}"
            )

        recorded_deadline = parse_review_deadline(reason)
        if recorded_deadline > EXPIRY:
            raise ValueError(
                f"{ADVISORY} review deadline {recorded_deadline:%Y-%m-%dT%H:%M:%SZ} "
                f"exceeds hard expiry {EXPIRY:%Y-%m-%dT%H:%M:%SZ}"
            )

        effective_expiry = min(EXPIRY, recorded_deadline)
        if now_utc >= effective_expiry:
            raise ValueError(
                f"{ADVISORY} exception expired at {effective_expiry:%Y-%m-%dT%H:%M:%SZ}"
            )


def main(policy_path: Path = Path("deny.toml"), now: dt.datetime | None = None) -> int:
    try:
        with Path(policy_path).open("rb") as policy_file:
            policy = tomllib.load(policy_file)
        current_time = dt.datetime.now(dt.timezone.utc) if now is None else now
        check(policy, current_time)
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"advisory expiry check failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
