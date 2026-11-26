#!/usr/bin/env python3
"""Report C0 capacity and confirmations from a redacted JSON summary on stdin."""

import json
import re
import sys


COUNT_FIELDS = (
    "hyperdrive_used",
    "access_apps_used",
    "access_apps_limit",
    "tunnels_used",
    "tunnels_limit",
)
CONFIRMATION_FIELDS = ("token_write_scope", "tunnel_verify_full")
MAX_INPUT_BYTES = 4096
MAX_REPLAY_BYTES = 16 * 1024


def unique_object(pairs):
    obj = dict(pairs)
    if len(obj) != len(pairs):
        raise ValueError("duplicate key")
    return obj


def read_summary():
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("oversized summary")
    data = json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object)
    if not isinstance(data, dict) or data.keys() != set(COUNT_FIELDS + CONFIRMATION_FIELDS):
        raise ValueError("invalid summary fields")
    for field in COUNT_FIELDS:
        value = data[field]
        if value is not None and (type(value) is not int or value < 0):
            raise ValueError("invalid count or limit")
    for field in CONFIRMATION_FIELDS:
        value = data[field]
        if value is not None and type(value) is not bool:
            raise ValueError("invalid confirmation")
    return data


def verified_hyperdrive_count(pages):
    if not isinstance(pages, list):
        return None
    seen = set()
    for number, page in enumerate(pages, 1):
        if not isinstance(page, dict) or page.get("success") is not True or page.get("errors") != []:
            return None
        info, results = page.get("result_info"), page.get("result")
        if not isinstance(info, dict) or not isinstance(results, list):
            return None
        current, width, total, count = (info.get(k) for k in ("page", "per_page", "total_count", "count"))
        if any(type(value) is not int for value in (current, width, total, count)):
            return None
        if current != number or not 1 <= width <= 100 or total < 0:
            return None
        if number == 1:
            expected_width, expected_total = width, total
        if (width, total) != (expected_width, expected_total):
            return None
        if count != len(results) or count != min(width, total - len(seen)):
            return None
        for config in results:
            config_id = config.get("id") if isinstance(config, dict) else None
            if not isinstance(config_id, str) or not re.fullmatch(r"[0-9a-f]{32}", config_id) or config_id in seen:
                return None
            seen.add(config_id)
        if len(seen) == total:
            return total if number == len(pages) else None
    return None


def replay_hyperdrive_count(path):
    try:
        with open(path, "rb") as replay:
            raw = replay.read(MAX_REPLAY_BYTES + 1)
        if len(raw) <= MAX_REPLAY_BYTES:
            return verified_hyperdrive_count(json.loads(raw.decode("utf-8"), object_pairs_hook=unique_object))
    except (ValueError, OSError, RecursionError):
        pass
    return None


def capacity(used, limit, needed, shortage):
    if used is None or limit is None:
        return "PENDING (count or limit unconfirmed)"
    status = "CAPACITY" if limit - used >= needed else shortage
    return f"{status} ({used} used of {limit}, {needed} needed)"


def confirmation(value, denied):
    return {True: "CONFIRMED", False: denied, None: "PENDING (unconfirmed)"}[value]


def main():
    args = sys.argv[1:]
    if args and (len(args) != 2 or args[0] != "--hyperdrive-pages"):
        print("Usage: staging_trial_c0_readback.py [--hyperdrive-pages SYNTHETIC_JSON]", file=sys.stderr)
        return 2
    try:
        data = read_summary()
    except (ValueError, OSError, RecursionError):
        print("Invalid C0 readback. Supply a redacted JSON summary as documented.", file=sys.stderr)
        return 2

    hyperdrive_used = replay_hyperdrive_count(args[1]) if args else data["hyperdrive_used"]
    print("Hyperdrive configs:", capacity(hyperdrive_used, 25, 4, "STOP"))
    print("Access apps:", capacity(data["access_apps_used"], data["access_apps_limit"], 2, "NO CAPACITY"))
    print("Dedicated tunnel:", capacity(data["tunnels_used"], data["tunnels_limit"], 1, "NO CAPACITY"))
    print("One token with Tunnel, Access and Hyperdrive write scope:", confirmation(
        data["token_write_scope"], "STOP (required scopes unavailable without widening a token)"
    ))
    print("Hyperdrive verify-full through Tunnel and Access:", confirmation(
        data["tunnel_verify_full"], "STOP (combined route does not support verify-full)"
    ))
    return 0


if __name__ == "__main__":
    sys.exit(main())
