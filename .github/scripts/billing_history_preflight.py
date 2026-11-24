#!/usr/bin/env python3
"""Compare prepared invoice projections with the staging archive, read-only."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys


SCRIPTS = Path(__file__).resolve().parent
API = SCRIPTS.parents[1] / "workers/api/src"
DATABASE = "nntunev2_staging"
MAX_SCOPE = 10_000
COUNTS = ("total", "exact", "eligible_hydration", "collision", "incomplete", "fresh")
BINDINGS = ("user_id", "stripe_invoice_id", "stripe_subscription_id", "plan_id",
            "start_s", "end_s", "amount_cents", "currency", "paid_s", "starts", "ends")


def comparison_sql():
    """Reuse the owned transaction's exact replay, coverage and hydration rules."""
    fold = (API / "billing_reconcile.rs").read_text()
    guard = (API / "billing_transaction_sql.rs").read_text()
    coverage, = re.findall(
        r'macro_rules! paid_invoice_coverage \{\s+\(\) => \{\s+"([^"]+)"', fold
    )
    exact, = re.findall(r'const MATCH_HISTORY_REPLAY: &str = concat!\(\s+"([^"]+)"', guard)
    hydration, = re.findall(
        r'"ON CONFLICT \(stripe_invoice_id\) DO UPDATE SET covered_periods = EXCLUDED.covered_periods'
        r'\s+WHERE ([^"]+)\s+RETURNING 1"', guard
    )

    def bind(sql):
        return re.sub(r"\$(\d+)", lambda match: "i." + BINDINGS[int(match[1]) - 1], sql)

    template = (SCRIPTS / "billing_history_preflight.sql").read_text()
    for marker, clause in {
        "@COVERAGE@": bind(coverage),
        "@EXACT_REPLAY@": bind(exact + coverage),
        "@HYDRATION@": hydration.replace("EXCLUDED.", "i."),
    }.items():
        if template.count(marker) != 1:
            raise ValueError("invalid comparison template")
        template = template.replace(marker, clause)
    return template


def run(role, expected, connection):
    if not re.fullmatch(r"[a-zA-Z0-9_.-]{1,63}", role) or not 1 <= expected <= MAX_SCOPE:
        raise ValueError("invalid comparison pins")
    result = subprocess.run(
        ["psql", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", f"database={DATABASE}",
         "-v", f"observer={role}", "-v", f"expected={expected}", "-v", f"limit={MAX_SCOPE + 1}"],
        input=comparison_sql(), env=connection, capture_output=True, text=True, timeout=20,
        check=False,
    )
    if result.returncode or result.stderr:
        raise ValueError("comparison refused")
    if result.stdout.strip() == "scope_incomplete":
        return 2, None

    def unique_object(pairs):
        value = dict(pairs)
        if len(value) != len(pairs):
            raise ValueError("invalid report")
        return value

    counts = json.loads(result.stdout, object_pairs_hook=unique_object)
    if (type(counts) is not dict or counts.keys() != set(COUNTS)
            or any(type(value) is not int or not 0 <= value <= MAX_SCOPE for value in counts.values())
            or counts["total"] != expected
            or sum(counts[key] for key in COUNTS[1:]) != expected):
        raise ValueError("invalid report")
    if counts["collision"]:
        return 1, counts
    if counts["incomplete"]:
        return 2, counts
    return 0, counts


def main():
    if os.environ.get("NNTUNE_INVOICE_PREFLIGHT") != "1":
        print("disabled", file=sys.stderr)
        return 3
    try:
        host = os.environ["BILLING_PREFLIGHT_HOST"]
        role = os.environ["BILLING_PREFLIGHT_ROLE"]
        password = os.environ["BILLING_PREFLIGHT_PASSWORD"]
        expected = int(os.environ["BILLING_PREFLIGHT_SCOPE_COUNT"])
        if (len(host) > 253 or not re.fullmatch(r"[a-zA-Z0-9-]+(?:\.[a-zA-Z0-9-]+)+", host)
                or not password):
            raise ValueError("invalid staging connection")
        status, counts = run(role, expected, {
            "PATH": os.defpath, "LC_ALL": "C", "PGCLIENTENCODING": "UTF8",
            "PGHOST": host, "PGPORT": "5432", "PGDATABASE": DATABASE, "PGUSER": role,
            "PGPASSWORD": password, "PGCONNECT_TIMEOUT": "5", "PGPASSFILE": "/dev/null",
            "PGSSLMODE": "verify-full", "PGSSLROOTCERT": "/etc/ssl/certs/ca-certificates.crt",
            "PGGSSENCMODE": "disable",
        })
    except (KeyError, ValueError, OSError, IndexError, RecursionError, subprocess.SubprocessError):
        print("refused", file=sys.stderr)
        return 3
    if counts is not None:
        print(json.dumps(counts, sort_keys=True))
    else:
        print("scope_incomplete", file=sys.stderr)
    return status


if __name__ == "__main__":
    raise SystemExit(main())
