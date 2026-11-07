#!/usr/bin/env python3
"""Ensure the spike bucket's `snap/` expiry rule before any upload.

Usage: lifecycle.py ensure <bucket>
Env: CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID.

Reads the bucket's lifecycle config as JSON from the R2 API (not
Wrangler's table output) and accepts it only if one rule carries the
name, the `snap/` prefix, a 14-day age expiry and `enabled`. Otherwise
it replaces that rule (other rules are kept), then reads back and checks
again. Exits non-zero if the rule cannot be confirmed.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.request

RULE = "expire-spike-snapshots"
PREFIX = "snap/"
MAX_AGE = 14 * 86400
API = "https://api.cloudflare.com/client/v4/accounts/{account}/r2/buckets/{bucket}/lifecycle"


def desired_rule() -> dict:
    return {
        "id": RULE,
        "enabled": True,
        "conditions": {"prefix": PREFIX},
        "deleteObjectsTransition": {"condition": {"type": "Age", "maxAge": MAX_AGE}},
    }


def rule_ok(config: dict) -> bool:
    """True if one enabled rule has the name, prefix and 14-day expiry."""
    for rule in config.get("rules") or []:
        if (
            rule.get("id") == RULE
            and rule.get("enabled") is True
            and (rule.get("conditions") or {}).get("prefix") == PREFIX
            and ((rule.get("deleteObjectsTransition") or {}).get("condition") or {})
            == {"type": "Age", "maxAge": MAX_AGE}
        ):
            return True
    return False


def repaired(config: dict) -> dict:
    """The config with our rule replaced by the desired one; others kept."""
    others = [r for r in config.get("rules") or [] if r.get("id") != RULE]
    return {"rules": others + [desired_rule()]}


def call(method: str, url: str, token: str, body: dict | None = None) -> dict:
    data = json.dumps(body).encode() if body is not None else None
    request = urllib.request.Request(url, data=data, method=method, headers={
        "authorization": f"Bearer {token}",
        "content-type": "application/json",
    })
    with urllib.request.urlopen(request, timeout=30) as response:
        payload = json.load(response)
    if not payload.get("success"):
        raise RuntimeError(f"{method} lifecycle: {payload.get('errors')}")
    return payload.get("result") or {}


def ensure(bucket: str) -> int:
    token = os.environ["CLOUDFLARE_API_TOKEN"]
    url = API.format(account=os.environ["CLOUDFLARE_ACCOUNT_ID"], bucket=bucket)
    config = call("GET", url, token)
    if rule_ok(config):
        return 0
    call("PUT", url, token, repaired(config))
    if rule_ok(call("GET", url, token)):
        return 0
    print(f"lifecycle: cannot confirm the {RULE} rule on {bucket}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] != "ensure":
        sys.exit("usage: lifecycle.py ensure <bucket>")
    sys.exit(ensure(sys.argv[2]))
