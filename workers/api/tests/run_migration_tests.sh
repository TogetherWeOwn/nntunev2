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
psql_run() { PGOPTIONS='-c search_path=public,pg_catalog' psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 "$@"; }

shopt -s nullglob
migrations=("$root"/migrations/*.sql)
tests=("$root"/workers/api/tests/sql/*.sql)
[ "${#migrations[@]}" -gt 0 ] || { echo "no migrations found" >&2; exit 1; }
[ "${#tests[@]}" -gt 0 ] || { echo "no sql tests found" >&2; exit 1; }

sql_dir="$root/workers/api/tests/sql"
pre_purge_tests=(legacy_vault_established.sql reconcile_legacy_vault_cohort.sql
  refresh_legacy_vault_cohort.sql library_artifacts.sql)
purge_seed="$sql_dir/purge_legacy_provider_credentials_seed.sql"
purge_check="$sql_dir/purge_legacy_provider_credentials_check.sql"

replay_migrations() {
  local f
  for f in "$@"; do
    # 0006 has an unconditional ADD COLUMN. The 0021 seed names
    # columns and a table that 0026 drops. Do not edit applied files.
    case "${f##*/}" in
      0006_account_deletion.sql|0021_billing_schema.sql) continue ;;
    esac
    psql_run -f "$f"
  done
}

check_purge_upgrade() {
  local migration=$1 fixture expected output
  for fixture in grants vault owner; do
    case "$fixture" in
      grants) expected='cache grants are not empty' ;;
      vault) expected='a vault owner lacks the account marker' ;;
      owner) expected='provider owners would lose reaping protection' ;;
    esac
    if output=$(psql_run -1 -v purge_case="$fixture" -f "$purge_seed" -f "$migration" 2>&1); then
      printf 'purge unexpectedly accepted %s\n' "$fixture" >&2
      return 1
    fi
    printf '%s\n' "$output"
    [[ "$output" == *"legacy provider purge refused: $expected"* ]] || return 1
  done
  # Exercise the real migration with legacy ciphertext and retained rows,
  # then roll back the successful fixture to leave a clean install.
  psql_run -v purge_case=allow -c 'BEGIN;' -f "$purge_seed" -f "$migration" \
    -f "$purge_check" -c 'ROLLBACK;'
}

replay=()
declare -A historical_test=()
for f in "${migrations[@]}"; do
  if [[ "${f##*/}" == 0051_purge_legacy_provider_credentials.sql ]]; then
    # The one-way contract requires the old relations to exist. Verify the
    # replay-safe prefix and its historical fixtures before removing them.
    replay_migrations "${replay[@]}"
    replay=()
    for fixture in "${pre_purge_tests[@]}"; do
      echo "test (pre-purge): $fixture"
      psql_run -f "$sql_dir/$fixture"
      historical_test["$sql_dir/$fixture"]=1
    done
    check_purge_upgrade "$f"
  else
    replay+=("$f")
  fi
  echo "migrate: ${f#"$root"/}"
  psql_run -f "$f"
done
replay_migrations "${replay[@]}"
for f in "${tests[@]}"; do
  [[ -n "${historical_test[$f]:-}" || "$f" == "$purge_seed" || "$f" == "$purge_check" ]] && continue
  echo "test:    ${f#"$root"/}"
  psql_run -v library_page_sql="$(<"$root/workers/api/src/library_page.sql")" \
    -v library_shared_upsert_sql="$(<"$root/workers/ingest/src/library_shared_upsert.sql")" \
    -v audio_manifest_store_sql="$(<"$root/workers/ingest/src/audio_manifest_store.sql")" \
    -v audio_classifications_store_sql="$(<"$root/workers/ingest/src/audio_classifications_store.sql")" -f "$f"
done
# Hosted-only concurrent fixtures are run separately by the migration CI job.
echo "migration tests passed"
