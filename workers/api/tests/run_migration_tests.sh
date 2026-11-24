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
psql_options='-c search_path=public,pg_catalog'
psql_command=(env "PGOPTIONS=$psql_options" psql "$DATABASE_URL")
session_fixture=()
if [[ ${GITHUB_ACTIONS:-} == true && ${RUNNER_ENVIRONMENT:-} == github-hosted ]]; then
  hosted_url='postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci'
  if [[ "$DATABASE_URL" != "$hosted_url" ]]; then
    printf 'disposable GitHub-hosted migration database required\n' >&2
    exit 1
  fi
  # Pin the address and clear libpq environment overrides before loading session fixtures.
  psql_command=(env -i "PATH=$PATH" "PGOPTIONS=$psql_options" psql "$hosted_url?hostaddr=127.0.0.1")
  session_fixture=(-v hosted_ingest_session_fixture=true)
fi
psql_run() { "${psql_command[@]}" -X -q -v ON_ERROR_STOP=1 "${session_fixture[@]}" "$@"; }

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
    # 0062 refuses pre-existing tables. Its hosted rehearsal tests replay
    # through the migration ledger.
    case "${f##*/}" in
      0006_account_deletion.sql|0021_billing_schema.sql|0062_ingest_sessions.sql) continue ;;
    esac
    psql_run -f "$f"
  done
}

check_purge_reader_contention() (
  local migration=$1 table=$2 reader_pid reader_out reader_in ready output
  # Hold the same lock as a reader in a separate session. The SELECT on
  # that session acknowledges lock acquisition before we attempt the drop.
  coproc PURGE_READER { psql_run -At; }
  reader_pid=$PURGE_READER_PID
  reader_out=${PURGE_READER[0]}
  reader_in=${PURGE_READER[1]}
  printf "BEGIN; SET LOCAL idle_in_transaction_session_timeout = '30s';\nLOCK TABLE public.%s IN ACCESS SHARE MODE NOWAIT;\nSELECT 'reader-ready';\n" "$table" >&"$reader_in"
  read -r -t 10 -u "$reader_out" ready
  [[ "$ready" == reader-ready ]]
  if output=$(psql_run -1 -v VERBOSITY=verbose -c "SET LOCAL lock_timeout = '2s';" -f "$migration" 2>&1); then
    printf 'purge unexpectedly accepted a reader on %s\n' "$table" >&2
    return 1
  fi
  printf 'purge with a reader on %s: %s\n' "$table" "$output"
  printf 'ROLLBACK;\n\\q\n' >&"$reader_in"
  wait "$reader_pid"
  # A queued drop also reports 55P03, but its message is a lock timeout.
  [[ "$output" == *'55P03: could not obtain lock on relation'* ]]
)

check_purge_upgrade() {
  local migration=$1 fixture expected output
  check_purge_reader_contention "$migration" users
  check_purge_reader_contention "$migration" cache_objects
  for fixture in grants vault owner owner_admin owner_deleting; do
    case "$fixture" in
      grants) expected='cache grants are not empty' ;;
      vault) expected='a vault owner lacks the account marker' ;;
      owner|owner_admin|owner_deleting) expected='provider owners would lose reaping protection' ;;
    esac
    if output=$(psql_run -1 -v purge_case="$fixture" -f "$purge_seed" -f "$migration" 2>&1); then
      printf 'purge unexpectedly accepted %s\n' "$fixture" >&2
      return 1
    fi
    printf 'purge guard (%s): %s\n' "$fixture" "$output"
    [[ "$output" == *"legacy provider purge refused: $expected"* ]] || return 1
  done
  # Exercise the real migration with legacy ciphertext and retained rows,
  # then roll back the successful fixture to leave a clean install.
  psql_run -v purge_case=allow -c 'BEGIN;' -f "$purge_seed" -f "$migration" \
    -f "$purge_check" -c 'ROLLBACK;'
}

check_invite_window_contention() (
  local writer_pid scheduler_out scheduler_in window_id output
  local overlapping='INSERT INTO public.invite_windows (starts_at, ends_at) VALUES (700, 800);'
  coproc SCHEDULER { psql_run -At; }
  writer_pid=$SCHEDULER_PID
  scheduler_out=${SCHEDULER[0]}
  scheduler_in=${SCHEDULER[1]}
  printf "BEGIN; SET LOCAL idle_in_transaction_session_timeout = '30s';\nINSERT INTO public.invite_windows (starts_at, ends_at) VALUES (650, 750) RETURNING id;\n" >&"$scheduler_in"
  if ! read -r -t 10 -u "$scheduler_out" window_id || [[ ! "$window_id" =~ ^[0-9]+$ ]]; then
    printf 'invite window writer did not return an id\n' >&2
    return 1
  fi
  # An overlapping writer must wait for the uncommitted schedule, then
  # receive an exclusion violation once the first schedule commits.
  if output=$(psql_run -1 -v VERBOSITY=verbose -c "SET LOCAL lock_timeout = '2s';" -c "$overlapping" 2>&1); then
    printf 'overlapping window unexpectedly committed concurrently\n' >&2
    return 1
  fi
  printf 'uncommitted invite window: %s\n' "$output"
  [[ "$output" == *'55P03:'* ]]
  printf 'COMMIT;\n\\q\n' >&"$scheduler_in"
  wait "$writer_pid"
  if output=$(psql_run -v VERBOSITY=verbose -c "$overlapping" 2>&1); then
    printf 'overlapping window unexpectedly committed on retry\n' >&2
    return 1
  fi
  printf 'committed invite window: %s\n' "$output"
  [[ "$output" == *'23P01:'*'invite_windows_no_overlap'* ]]
  psql_run -c "DELETE FROM public.invite_windows WHERE id = $window_id;"
)

replay=()
declare -A historical_test=()
for f in "${migrations[@]}"; do
  if [[ "${f##*/}" == 0053_purge_legacy_provider_credentials.sql ]]; then
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
  if [[ "${f##*/}" == 0057_invite_windows.sql ]]; then
    echo 'test (upgrade/replay): invite_windows_upgrade.sql'
    psql_run -v migration="$f" -f "$root/workers/api/tests/fixtures/invite_windows_upgrade.sql"
  fi
  echo "migrate: ${f#"$root"/}"
  psql_run -f "$f"
  if [[ "${f##*/}" == 0057_invite_windows.sql ]]; then
    echo 'test (concurrent schedules): invite_windows'
    check_invite_window_contention
  fi
done
replay_migrations "${replay[@]}"
for f in "${tests[@]}"; do
  [[ -n "${historical_test[$f]:-}" || "$f" == "$purge_seed" || "$f" == "$purge_check" ]] && continue
  echo "test:    ${f#"$root"/}"
  psql_run -v library_page_sql="$(<"$root/workers/api/src/library_page.sql")" \
    -v library_shared_upsert_sql="$(<"$root/workers/ingest/src/library_shared_upsert.sql")" \
    -v audio_manifest_store_sql="$(<"$root/workers/ingest/src/audio_manifest_store.sql")" \
    -v storage_digest_claim_lookup_sql="$(<"$root/workers/api/src/storage_digest_claim_lookup.sql")" \
    -v storage_digest_read_lookup_sql="$(<"$root/workers/api/src/storage_digest_read_lookup.sql")" \
    -v observe_invite_expiry_sql="$(<"$root/workers/api/src/observe_invite_expiry.sql")" \
    -v private_listening_access_sql="$(<"$root/workers/api/src/private_listening_access.sql")" \
    -v lock_invite_issuer_sql="$(<"$root/workers/api/src/lock_invite_issuer_for_sanction.sql")" \
    -v sanction_invite_issuer_sql="$(<"$root/workers/api/src/sanction_invite_issuer.sql")" \
    -v ingest_session_reserve_sql="$(<"$root/workers/api/src/ingest_session_reserve.sql")" \
    -v ingest_session_terminate_sql="$(<"$root/workers/api/src/ingest_session_terminate.sql")" \
    -v ingest_session_acquire_write_lease_sql="$(<"$root/workers/api/src/ingest_session_acquire_write_lease.sql")" \
    -v ingest_session_advance_checkpoint_sql="$(<"$root/workers/api/src/ingest_session_advance_checkpoint.sql")" \
    -v ingest_session_status_sql="$(<"$root/workers/api/src/ingest_session_status.sql")" \
    -v ingest_session_claim_cleanup_sql="$(<"$root/workers/api/src/ingest_session_claim_cleanup.sql")" \
    -v ingest_session_ack_cleanup_enqueue_sql="$(<"$root/workers/api/src/ingest_session_ack_cleanup_enqueue.sql")" \
    -v audio_classifications_store_sql="$(<"$root/workers/ingest/src/audio_classifications_store.sql")" -f "$f"
done
# Hosted-only concurrent fixtures are run separately by the migration CI job.
echo "migration tests passed"
