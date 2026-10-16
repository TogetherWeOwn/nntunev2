#!/usr/bin/env bash
# Applies every migrations/*.sql in order to an EMPTY Postgres database,
# then runs each workers/api/tests/sql/*.sql regression script against the
# result. Fail-closed: no DATABASE_URL, no psql, or any SQL error is a
# failure (nothing is silently skipped).
#
# DATABASE_URL must point at a scratch database that this script may fill
# (CI's postgres service container). Never point it at a shared database.
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL (scratch Postgres) is required}"
command -v psql >/dev/null || { echo "psql not found" >&2; exit 1; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
psql_run() { psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 "$@"; }

shopt -s nullglob
migrations=("$root"/migrations/*.sql)
tests=("$root"/workers/api/tests/sql/*.sql)
[ "${#migrations[@]}" -gt 0 ] || { echo "no migrations found" >&2; exit 1; }
[ "${#tests[@]}" -gt 0 ] || { echo "no sql tests found" >&2; exit 1; }

for f in "${migrations[@]}"; do
  echo "migrate: ${f#"$root"/}"
  psql_run -f "$f"
done
# Migrations are idempotent: a second pass must be a clean no-op.
for f in "${migrations[@]}"; do
  psql_run -f "$f"
done
for f in "${tests[@]}"; do
  echo "test:    ${f#"$root"/}"
  psql_run -f "$f"
done
echo "migration tests passed"
