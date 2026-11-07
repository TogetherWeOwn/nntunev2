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
# Pin the session search_path for every apply: pre-convention migrations set
# no search_path of their own, so without this pin an unqualified CREATE TABLE
# resolves wherever the caller's role points. This wrapper (not a one-off) is
# the required way to apply any pre-convention file in every environment;
# new migrations carry their own in-file SET (see migrations/README.md).
psql_run() { PGOPTIONS='-c search_path=public,pg_catalog' psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -v migrations_dir="$root/migrations" "$@"; }

shopt -s nullglob
migrations=("$root"/migrations/*.sql)
tests=("$root"/workers/api/tests/sql/*.sql)
[ "${#migrations[@]}" -gt 0 ] || { echo "no migrations found" >&2; exit 1; }
[ "${#tests[@]}" -gt 0 ] || { echo "no sql tests found" >&2; exit 1; }

for f in "${migrations[@]}"; do
  echo "migrate: ${f#"$root"/}"
  psql_run -f "$f"
done
# Reapply replay-safe migrations to prove they are clean no-ops. Migration 0006
# predates this check and has an unconditional ADD COLUMN; applied migrations
# are immutable, so it is intentionally excluded from the replay pass.
# Migration 0021 is excluded for a different reason: migration 0026 drops
# columns its seed INSERT names (and the usage_ledger table it creates), and
# applied migrations cannot be edited, so 0021 cannot re-run after 0026.
for f in "${migrations[@]}"; do
  [[ "${f##*/}" == "0006_account_deletion.sql" ]] && continue
  [[ "${f##*/}" == "0021_billing_schema.sql" ]] && continue
  psql_run -f "$f"
done
for f in "${tests[@]}"; do
  echo "test:    ${f#"$root"/}"
  psql_run -v library_page_sql="$(<"$root/workers/api/src/library_page.sql")" -f "$f"
done
echo "migration tests passed"
