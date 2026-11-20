"""Read one uncached, aggregate-only PostgreSQL pressure sample for staging."""

import datetime as dt
import os
import re
import subprocess
import time
from typing import NamedTuple
from urllib.parse import parse_qs, unquote, urlsplit

ROLE = "nntune_staging_observer"
DATABASE = "postgres"
CONNECT_SECONDS = 2
DEADLINE_SECONDS = 5
MAX_SAMPLE_AGE_SECONDS = 3
MEASUREMENT_SLOTS = 2

SQL = rf"""BEGIN READ ONLY;
SELECT current_user = '{ROLE}' AND current_database() = '{DATABASE}' AS observer_identity
\gset
\if :observer_identity
SELECT floor(extract(epoch FROM clock_timestamp()))::bigint;
SELECT * FROM observer.connection_counts();
\else
\echo wrong-observer-identity
\endif
COMMIT;
"""


class UnsafeSample(Exception):
    pass


class Sample(NamedTuple):
    timestamp: int
    measurement: int
    clients: int
    observer: int
    maximum: int
    reserved: int

    @property
    def ordinary_free(self):
        # The backend running this SELECT is already in `clients`.
        return self.maximum - self.reserved - self.clients


def connection_env(env):
    try:
        url = urlsplit(env.get("PLANETSCALE_OBSERVER_URL_STAGING", ""))
        query = parse_qs(url.query, strict_parsing=True, keep_blank_values=True)
        password = unquote(url.password or "", errors="strict")
        host = url.hostname or ""
        port = 5432 if url.port is None else url.port
        if (
            url.scheme not in ("postgres", "postgresql")
            or not re.fullmatch(r"[a-z0-9-]+(?:\.[a-z0-9-]+)+", host)
            or not 1 <= port <= 65535
            or unquote(url.username or "") != ROLE
            or unquote(url.path) != f"/{DATABASE}"
            or not password
            or "\0" in password
            or url.fragment
            or set(query) - {"sslmode", "sslrootcert"}
            or query.get("sslmode", ["verify-full"]) != ["verify-full"]
            or query.get("sslrootcert", ["system"]) != ["system"]
        ):
            raise ValueError
    except ValueError:
        raise UnsafeSample("config") from None
    return {
        "PATH": env.get("PATH", os.defpath),
        "LC_ALL": "C",
        "PGHOST": host,
        "PGPORT": str(port),
        "PGDATABASE": DATABASE,
        "PGUSER": ROLE,
        "PGPASSWORD": password,
        "PGPASSFILE": "/dev/null",
        "PGSSLMODE": "verify-full",
        "PGSSLROOTCERT": "/etc/ssl/certs/ca-certificates.crt",
        "PGGSSENCMODE": "disable",
        "PGCONNECT_TIMEOUT": str(CONNECT_SECONDS),
        "PGOPTIONS": "-c default_transaction_read_only=on -c statement_timeout=1000 -c lock_timeout=1000 -c search_path=pg_catalog",
    }


def parse_sample(raw, since, now):
    if raw == b"wrong-observer-identity\n":
        raise UnsafeSample("identity")
    # Function result columns, in order: measurement, total client, observer,
    # max_connections, reserved (the sum of all reserved slots).
    match = re.fullmatch(rb"([0-9]{1,12})\n([0-9]{1,9}(?:\|[0-9]{1,9}){4})\n", raw)
    if not match:
        raise UnsafeSample("shape")
    row = Sample(int(match[1]), *map(int, match[2].split(b"|")))
    # Whole seconds from the same transaction, with one second for clock skew.
    if (
        now < since
        or not int(since) - 1 <= row.timestamp <= int(now) + 1
        or now - row.timestamp > MAX_SAMPLE_AGE_SECONDS
    ):
        raise UnsafeSample("stale")
    if (
        row.observer != 1
        or not row.measurement + row.observer <= row.clients <= row.maximum
        or not 0 <= row.reserved < row.maximum
    ):
        raise UnsafeSample("counts")
    return row


def sample(env, run=subprocess.run, wall=time.time, monotonic=time.monotonic):
    child = connection_env(env)
    started = monotonic()
    since = wall()
    try:
        result = run(
            [
                "psql",
                "--no-psqlrc",
                "--no-password",
                "--quiet",
                "--tuples-only",
                "--no-align",
                "--field-separator=|",
                "--set=ON_ERROR_STOP=1",
            ],
            input=SQL.encode("ascii"),
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            timeout=DEADLINE_SECONDS,
            env=child,
        )
    except subprocess.TimeoutExpired:
        raise UnsafeSample("deadline") from None
    except (OSError, ValueError):
        raise UnsafeSample("client") from None
    if monotonic() - started > DEADLINE_SECONDS:
        raise UnsafeSample("deadline")
    if result.returncode != 0:
        raise UnsafeSample("query")
    return parse_sample(result.stdout, since, wall())


def require_preflight(row):
    if row.measurement != 0 or row.ordinary_free < MEASUREMENT_SLOTS:
        raise UnsafeSample("pressure")


def main():
    try:
        row = sample(os.environ)
        require_preflight(row)
    except UnsafeSample as exc:
        print(f"staging-observer: UNSAFE category={exc}")
        return 1
    sampled_at = dt.datetime.fromtimestamp(row.timestamp, dt.UTC).isoformat()
    print(
        f"staging-observer: PASS (point-in-time observer smoke only) sampled_at={sampled_at} "
        f"measurement={row.measurement} clients={row.clients} observer={row.observer} "
        f"maximum={row.maximum} reserved={row.reserved} ordinary_free={row.ordinary_free}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
