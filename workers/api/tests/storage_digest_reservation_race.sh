#!/usr/bin/env bash
# Only the GitHub-hosted migration job's ephemeral service is supported.
set -euo pipefail
if [[ $# != 0 || ${GITHUB_ACTIONS:-} != true || ${RUNNER_ENVIRONMENT:-} != github-hosted || ${DATABASE_URL:-} != 'postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci' ]]; then
  printf 'disposable GitHub-hosted migration database required\n' >&2; exit 1
fi
# Ignore inherited libpq configuration, including service files and host overrides.
dir="$(mktemp -d)"
trap 'jobs -pr | xargs -r kill; wait || :; rm -rf -- "$dir"' EXIT
psql_run() { env -i PATH="$PATH" PGAPPNAME="${PGAPPNAME:-}" PGOPTIONS='-c search_path=public,pg_catalog -c statement_timeout=30000' psql 'postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci?hostaddr=127.0.0.1' -X -q -v ON_ERROR_STOP=1 "$@"; }

psql_run <<'SQL'
INSERT INTO public.storage_environments VALUES
 ('01000000-0000-4000-8000-000000000001'), ('01000000-0000-4000-8000-000000000002');
INSERT INTO public.users (id, email, created_at) VALUES ('digest-test-owner', 'digest-test@example.com', 1700000000);
INSERT INTO public.storage_library_namespaces VALUES
 ('11000000-0000-4000-8000-000000000001','01000000-0000-4000-8000-000000000001',NULL),
 ('11000000-0000-4000-8000-000000000002','01000000-0000-4000-8000-000000000002',NULL),
 ('11000000-0000-4000-8000-000000000003','01000000-0000-4000-8000-000000000001','digest-test-owner');
INSERT INTO public.storage_object_intents
SELECT ('21000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,
       ('31000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,
       CASE WHEN i=9 THEN '01000000-0000-4000-8000-000000000002' ELSE '01000000-0000-4000-8000-000000000001' END::uuid,
       CASE WHEN i=9 THEN '11000000-0000-4000-8000-000000000002' WHEN i=10 THEN '11000000-0000-4000-8000-000000000003' ELSE '11000000-0000-4000-8000-000000000001' END::uuid,
       '41000000-0000-4000-8000-000000000001', CASE WHEN i=12 THEN 2 ELSE 1 END, CASE WHEN i=12 THEN 66 ELSE 65 END, decode(repeat('ab',48),'hex')
FROM generate_series(1,14) AS i;
INSERT INTO public.storage_plaintext_digest_claim_intents
SELECT object_id,environment_id,library_id,plaintext_bytes,decode(repeat(CASE WHEN object_id::text LIKE '%000000000011' THEN 'cd' WHEN object_id::text LIKE '%000000000013' OR object_id::text LIKE '%000000000014' THEN 'ef' ELSE 'ab' END,32),'hex')
FROM public.storage_object_intents WHERE object_id::text LIKE '21000000-%';
SQL

# Separate database sessions contend for the same asserted identity.
pids=()
for i in {1..8}; do
  id="$(printf '21000000-0000-4000-8000-%012d' "$i")"
  psql_run -Atc "SELECT coalesce(public.reserve_storage_digest('$id', 3600)::text, 'lost')" >"$dir/$i" &
  pids+=("$!")
done
for pid in "${pids[@]}"; do wait "$pid"; done
winners=0
for i in {1..8}; do
  result="$(<"$dir/$i")"
  case "$result" in 1) ((winners+=1));; lost) :;; *) printf 'unexpected result: %s\n' "$result" >&2; exit 1;; esac
done
[[ "$winners" == 1 ]] || { printf 'expected one winner, got %s\n' "$winners" >&2; exit 1; }

psql_run <<'SQL'
DO $$
DECLARE old_holder UUID; next_holder UUID; old_generation BIGINT; new_generation BIGINT;
BEGIN
 SELECT object_id,generation INTO STRICT old_holder,old_generation FROM public.storage_digest_reservations;
 SELECT object_id INTO STRICT next_holder FROM public.storage_plaintext_digest_claim_intents
 WHERE object_id BETWEEN '21000000-0000-4000-8000-000000000001' AND '21000000-0000-4000-8000-000000000008' AND object_id<>old_holder LIMIT 1;
 IF public.reserve_storage_digest(next_holder,3600) IS NOT NULL THEN RAISE EXCEPTION 'active holder lost'; END IF;
 IF public.reserve_storage_digest('21000000-0000-4000-8000-000000000099',3600) IS NOT NULL THEN RAISE EXCEPTION 'unknown claim reserved'; END IF;
 IF public.reserve_storage_digest('21000000-0000-4000-8000-000000000009',3600) IS DISTINCT FROM 1 THEN RAISE EXCEPTION 'environment blocked'; END IF;
 FOR i IN 10..12 LOOP
   IF public.reserve_storage_digest(('21000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,3600) IS DISTINCT FROM 1 THEN
     RAISE EXCEPTION 'library, digest or length blocked: %', i;
   END IF;
 END LOOP;
 -- Expire without wall-clock sleeps; preserve the row and its fencing generation.
 UPDATE public.storage_digest_reservations SET expires_at=clock_timestamp()-interval '1 second' WHERE object_id=old_holder;
 IF public.release_storage_digest(old_holder,old_generation) THEN RAISE EXCEPTION 'expired holder released'; END IF;
 new_generation := public.reserve_storage_digest(next_holder,3600);
 IF new_generation IS DISTINCT FROM old_generation+1 THEN RAISE EXCEPTION 'generation not advanced'; END IF;
 IF public.release_storage_digest(old_holder,old_generation) THEN RAISE EXCEPTION 'stale holder released successor'; END IF;
 IF public.release_storage_digest(next_holder,old_generation) THEN RAISE EXCEPTION 'wrong generation released'; END IF;
 IF NOT public.release_storage_digest(next_holder,new_generation) THEN RAISE EXCEPTION 'failure release failed'; END IF;
 IF public.release_storage_digest(next_holder,new_generation) THEN RAISE EXCEPTION 'release repeated'; END IF;
 IF public.reserve_storage_digest(old_holder,3600) IS DISTINCT FROM new_generation+1 THEN RAISE EXCEPTION 'released claim not reacquired'; END IF;
 BEGIN
   UPDATE public.storage_digest_reservations SET generation=1 WHERE object_id=old_holder;
   RAISE EXCEPTION 'generation rollback accepted';
 EXCEPTION WHEN check_violation THEN NULL; END;
 BEGIN
   UPDATE public.storage_digest_reservations SET plaintext_bytes=plaintext_bytes+1 WHERE object_id=old_holder;
   RAISE EXCEPTION 'key change accepted';
 EXCEPTION WHEN check_violation THEN NULL; END;
 BEGIN
   DELETE FROM public.storage_digest_reservations;
   RAISE EXCEPTION 'delete accepted';
 EXCEPTION WHEN check_violation THEN NULL; END;
 BEGIN
   PERFORM public.reserve_storage_digest(old_holder,0);
   RAISE EXCEPTION 'invalid duration accepted';
 EXCEPTION WHEN invalid_parameter_value THEN NULL; END;
END $$;
SQL

# Hold an uncommitted first insert until a second session is demonstrably blocked.
mkfifo "$dir/input"
exec 3<>"$dir/input"
psql_run <"$dir/input" >"$dir/holder" &
holder=$!
printf "BEGIN; SELECT public.reserve_storage_digest('21000000-0000-4000-8000-000000000013', 1);\n\\! touch '%s/ready'\n" "$dir" >&3
for _ in {1..100}; do [[ -e "$dir/ready" ]] && break; sleep 0.1; done
[[ -e "$dir/ready" ]] || { printf 'holder did not start\n' >&2; exit 1; }
PGAPPNAME=digest_contender psql_run -Atc "BEGIN; SELECT public.reserve_storage_digest('21000000-0000-4000-8000-000000000014', 3); SELECT expires_at > clock_timestamp() FROM public.storage_digest_reservations WHERE object_id='21000000-0000-4000-8000-000000000014'; COMMIT" >"$dir/contender" &
contender=$!
blocked=0
for _ in {1..100}; do
  if [[ "$(psql_run -Atc "SELECT count(*) FROM pg_stat_activity WHERE application_name='digest_contender' AND wait_event_type='Lock'")" == 1 ]]; then blocked=1; break; fi
  sleep 0.1
done
[[ "$blocked" == 1 ]] || { printf 'contender did not block\n' >&2; exit 1; }
# The wait exceeds the lease duration, then rollback removes the conflicting insert.
psql_run -c 'SELECT pg_sleep(4)' >/dev/null
printf 'ROLLBACK;\n\\q\n' >&3
exec 3>&-
wait "$holder"
wait "$contender"
[[ "$(<"$dir/contender")" == $'1\nt' ]] || { printf 'rollback contender did not win an unexpired lease\n' >&2; exit 1; }

tests_dir="$(dirname -- "${BASH_SOURCE[0]}")"
psql_run -v storage_digest_claim_lookup_sql="$(<"$tests_dir/../src/storage_digest_claim_lookup.sql")" \
  -v storage_digest_read_lookup_sql="$(<"$tests_dir/../src/storage_digest_read_lookup.sql")" \
  -f "$tests_dir/fixtures/storage_digest_reference.sql"
# Keep the reference-key expiry cases and both claim/delete lock orders.
# Then use the committed references on 9 and 12 for the read/detach controls.
for scenario in '11 false ROLLBACK f - 12' '11 false ROLLBACK t - 12' '9 false COMMIT f false' '10 true COMMIT f true' \
  '11 false ROLLBACK t true' '12 true ROLLBACK t false' '9 detach COMMIT f false' '12 detach ROLLBACK t false'; do
  read -r id deleting finish expected tombstone other <<<"$scenario"
  object="$(printf '21000000-0000-4000-8000-%012d' "$id")"
  short_id="$(printf '%02d' "$id")"
  pins="$object|media-$short_id|{page-$short_id}"
  lookup="SELECT * FROM public.digest_lookup_fixture($id)"
  delete="SELECT public.digest_ref_fixture($id, true)"
  holder_expected=t; contender_expected="$expected"; deadline=infinity
  if [[ -n "$other" ]]; then
    holder_query="SELECT public.digest_ref_fixture($id, false)"
    duration='1 minute'; [[ "$expected" != f ]] || duration='8 seconds'
    deadline="$(psql_run -Atc "SELECT clock_timestamp() + interval '$duration'")"
    query="SELECT public.digest_ref_fixture($other, false, '', '$object', '$deadline')"
  elif [[ "$deleting" == detach ]]; then
    holder_query="SELECT 1 FROM public.storage_validated_digest_winners WHERE object_id='$object' FOR UPDATE"
    query="SELECT * FROM public.digest_read_lookup_fixture($id, clock_timestamp() + interval '5 minutes')"
    holder_expected=$'1\n1'; contender_expected=''
    if [[ "$expected" == t ]]; then contender_expected="$pins"; fi
  elif [[ "$deleting" == true ]]; then
    holder_query="$delete"; query="$lookup"; contender_expected=''
    if [[ "$expected" == t ]]; then contender_expected="$pins"; fi
  else
    holder_query="$lookup"; query="$delete"; holder_expected="$pins"
  fi
  mkfifo "$dir/ref-$scenario"; exec 3<>"$dir/ref-$scenario"
  PGAPPNAME=digest_ref_holder psql_run -At <"$dir/ref-$scenario" >"$dir/ref-holder" & holder=$!
  printf "BEGIN ISOLATION LEVEL READ COMMITTED; %s;\n\\! touch '%s/ready-%s'\n" "$holder_query" "$dir" "$scenario" >&3
  for _ in {1..100}; do [[ -e "$dir/ready-$scenario" ]] && break; sleep 0.1; done
  [[ -e "$dir/ready-$scenario" ]] || exit 1
  PGAPPNAME=digest_ref_contender psql_run -Atc "$query" >"$dir/ref-contender" & contender=$!
  blocked=0
  for _ in {1..100}; do
    if [[ "$(psql_run -Atc "SELECT count(*) FROM pg_stat_activity c JOIN pg_stat_activity h ON h.pid=ANY(pg_blocking_pids(c.pid)) WHERE c.application_name='digest_ref_contender' AND h.application_name='digest_ref_holder' AND c.wait_event='transactionid' AND clock_timestamp() < '$deadline'")" == 1 ]]; then blocked=1; break; fi
    sleep 0.1
  done
  [[ "$blocked" == 1 ]] || { printf 'reference contender did not block before deadline: %s\n' "$scenario" >&2; exit 1; }
  if [[ "$deleting" == detach ]]; then
    # The holder owns only the winner lock at this point. A reference-first
    # lookup would block this delete while still waiting for the winner.
    printf "%s\n" "SET LOCAL lock_timeout = '2s';
      WITH detached AS (DELETE FROM public.storage_validated_digest_references
                        WHERE reference_id='$object' AND object_id='$object' RETURNING 1)
      SELECT count(*) FROM detached;" >&3
  fi
  if [[ -n "$other" && "$expected" == f ]]; then psql_run -c "SELECT pg_sleep_until('$deadline')" >/dev/null; fi
  printf '%s;\n\\q\n' "$finish" >&3; exec 3>&-
  wait "$holder"; wait "$contender"
  [[ "$(<"$dir/ref-holder")" == "$holder_expected" && "$(<"$dir/ref-contender")" == "$contender_expected" ]] || { printf 'unexpected reference race result: %s\n' "$scenario" >&2; exit 1; }
  if [[ -n "$other" ]]; then
    target=none
    if [[ "$expected" == t ]]; then target="$(printf '21000000-0000-4000-8000-%012d' "$other")"; fi
    [[ "$(psql_run -Atc "SELECT coalesce(string_agg(object_id::text, ','), 'none') FROM public.storage_validated_digest_references WHERE reference_id='$object'")" == "$target" ]] || exit 1
    # Remove the verified synthetic reference so the winner-lock cases can reuse the objects.
    psql_run -c "DELETE FROM public.storage_validated_digest_references WHERE reference_id='$object'"
    printf 'reference-key lock case passed: holder rolled back; contender=%s committed reference target=%s\n' "$expected" "$target"
  elif [[ "$deleting" == detach ]]; then
    [[ "$(psql_run -Atc "SELECT deleting_at IS NULL, read_until > clock_timestamp(), EXISTS (SELECT FROM public.storage_validated_digest_references WHERE reference_id='$object' AND object_id='$object') FROM public.storage_validated_digest_winners WHERE object_id='$object'")" == "t|$expected|$expected" ]] || exit 1
    if [[ "$expected" == f ]]; then
      [[ "$(psql_run -Atc "$delete")" == t ]] || exit 1
    else
      psql_run -c "BEGIN; SELECT 1 FROM public.storage_validated_digest_winners WHERE object_id='$object' FOR UPDATE; DELETE FROM public.storage_validated_digest_references WHERE reference_id='$object'; COMMIT" >/dev/null
      [[ "$(psql_run -Atc "$delete")" == f ]] || exit 1
    fi
    printf 'read lookup/detach lock case passed: detach=%s committed-read=%s\n' "$finish" "$expected"
  else
    # Any pins seen before ROLLBACK are provisional. Verify the committed
    # outcome has either the exact reference or a tombstone with no references.
    [[ "$(psql_run -Atc "SELECT (deleting_at IS NOT NULL) = $tombstone AND ARRAY(SELECT r.reference_id FROM public.storage_validated_digest_references r WHERE r.object_id=w.object_id) = CASE WHEN $tombstone THEN ARRAY[]::uuid[] ELSE ARRAY[w.object_id] END FROM public.storage_validated_digest_winners w WHERE object_id='$object'")" == t ]] || exit 1
    printf 'claim lookup/delete lock case passed: delete-first=%s holder=%s contender-won=%s\n' "$deleting" "$finish" "$expected"
  fi
done
psql_run <<'SQL'
DO $$ DECLARE attempt record; original jsonb; BEGIN
    FOR attempt IN SELECT * FROM (VALUES ('01','quarantined=false'), ('10','deleting_at=NULL'), ('10','deleting_at=''infinity''')) AS changes(suffix, assignment) LOOP
        SELECT to_jsonb(w) INTO STRICT original FROM public.storage_validated_digest_winners w WHERE right(object_id::text,2)=attempt.suffix;
        BEGIN
            EXECUTE format('UPDATE public.storage_validated_digest_winners SET %s WHERE right(object_id::text,2)=%L', attempt.assignment, attempt.suffix);
            RAISE EXCEPTION 'retirement mutation accepted: %', attempt.assignment;
        EXCEPTION WHEN check_violation THEN NULL; END;
        IF (SELECT to_jsonb(w) FROM public.storage_validated_digest_winners w WHERE right(object_id::text,2)=attempt.suffix) IS DISTINCT FROM original THEN
            RAISE EXCEPTION 'rejected retirement mutation changed state: %', attempt.assignment;
        END IF;
        RAISE NOTICE 'committed retirement preserved: %', attempt.assignment;
    END LOOP;
END $$;
DROP FUNCTION public.digest_read_lookup_fixture(int, timestamptz);
DROP FUNCTION public.digest_read_statement(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID, TIMESTAMPTZ);
DROP FUNCTION public.digest_lookup_fixture(int);
DROP FUNCTION public.digest_claim_statement(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID);
DROP FUNCTION public.digest_ref_fixture(int, bool, text, uuid, timestamptz);
SQL

# Reuse the disposable lookup fixture's namespace and owner, not its digest
# winners. Load the inert schema only here, then run the actual SQL statements.
psql_run -f "$tests_dir/../src/ingest_session_schema.sql"
reserve="PREPARE reserve_session(uuid, uuid, text, text, bytea, uuid) AS $(<"$tests_dir/../src/ingest_session_reserve.sql")"
abort="PREPARE abort_session(uuid, uuid, text, uuid, text) AS $(<"$tests_dir/../src/ingest_session_terminate.sql")"
advance="PREPARE advance_checkpoint(uuid, uuid, text, uuid, bytea, uuid, bigint, bigint, bigint) AS $(<"$tests_dir/../src/ingest_session_advance_checkpoint.sql")"
scope="'01000000-0000-4000-8000-000000000001', '11000000-0000-4000-8000-000000000001', 'digest-test-owner'"
fp="decode(repeat('de', 32), 'hex')"
gen="'41000000-0000-4000-8000-000000000001'"
# Exercise both lock orders and both possible holder dispositions. '-' denotes
# a successful statement returning zero rows, not a database/transport error.
for scenario in 'abort COMMIT aborted - 0 0' 'abort ROLLBACK aborted 1 1 100' \
                'checkpoint COMMIT 1 aborted 1 100' 'checkpoint ROLLBACK 1 aborted 0 0' \
                'checkpoint ROLLBACK 1 expired 0 0 expire'; do
  read -r first finish holder_expected contender_expected rev offset action <<<"$scenario"
  action="${action:-abort}"
  initial_idle="absolute_deadline - interval '23 hours 55 minutes'"
  if [[ "$action" == expire ]]; then
    initial_idle="absolute_deadline - interval '23 hours 59 minutes 52 seconds'"
  fi
  sid="$(psql_run -Atc 'SELECT gen_random_uuid()')"
  [[ "$sid" =~ ^[0-9a-f-]{36}$ ]]
  [[ "$(psql_run -Atc "$reserve" -c "EXECUTE reserve_session($scope, 'race-$action-$first-$finish', $fp, '$sid')")" == "$sid" ]]
  psql_run -c "UPDATE public.ingest_session_reservations SET idle_deadline=$initial_idle WHERE id='$sid'"
  abort_query="EXECUTE abort_session($scope, '$sid', 'abort');"
  advance_query="EXECUTE advance_checkpoint($scope, '$sid', $fp, $gen, 0, 0, 100);"
  holder_prepare="$abort"; holder_query="$abort_query"
  contender_prepare="$advance"; contender_query="$advance_query"
  terminal=aborted
  if [[ "$first" == checkpoint ]]; then
    holder_prepare="$advance"; holder_query="$advance_query"
    contender_prepare="$abort"; contender_query="$abort_query"
  elif [[ "$finish" == ROLLBACK ]]; then
    terminal=active
  fi
  if [[ "$action" == expire ]]; then
    contender_query="EXECUTE abort_session($scope, '$sid', 'expire');"
    terminal=expired
  fi

  race="$dir/session-$action-$first-$finish"
  mkdir -- "$race"
  mkfifo "$race/input"; exec 3<>"$race/input"
  PGAPPNAME=ingest_session_holder psql_run -At <"$race/input" >"$race/holder" & holder=$!
  printf "BEGIN ISOLATION LEVEL READ COMMITTED; SET LOCAL idle_in_transaction_session_timeout = '30s';\n%s\n%s\n\\! touch '%s/ready'\n" \
    "$holder_prepare" "$holder_query" "$race" >&3
  for _ in {1..100}; do [[ -e "$race/ready" ]] && break; sleep 0.1; done
  [[ -e "$race/ready" ]] || { printf 'ingest session holder did not start: %s\n' "$scenario" >&2; exit 1; }
  PGAPPNAME=ingest_session_contender psql_run -Atc "$contender_prepare" \
    -c 'BEGIN ISOLATION LEVEL READ COMMITTED;' -c "$contender_query" -c 'COMMIT;' \
    3>&- >"$race/contender" & contender=$!
  blocked=0
  for _ in {1..100}; do
    if [[ "$(psql_run -Atc "SELECT count(*) FROM pg_stat_activity c JOIN pg_stat_activity h ON h.pid=ANY(pg_blocking_pids(c.pid)) WHERE c.application_name='ingest_session_contender' AND h.application_name='ingest_session_holder' AND c.wait_event='transactionid'")" == 1 ]]; then blocked=1; break; fi
    sleep 0.1
  done
  [[ "$blocked" == 1 ]] || { printf 'ingest session contender did not wait on holder: %s\n' "$scenario" >&2; exit 1; }
  if [[ "$action" == expire ]]; then
    # The observer sees the original deadline until the checkpoint rolls back.
    expired_sql="SELECT clock_timestamp() >= idle_deadline FROM public.ingest_session_reservations WHERE id='$sid'"
    [[ "$(psql_run -Atc "$expired_sql")" == f ]] || { printf 'expiry contender arrived after the idle cutoff\n' >&2; exit 1; }
    expired=0
    for _ in {1..100}; do
      if [[ "$(psql_run -Atc "$expired_sql")" == t ]]; then expired=1; break; fi
      sleep 0.1
    done
    [[ "$expired" == 1 ]] || { printf 'idle cutoff did not pass while the contender waited\n' >&2; exit 1; }
  fi
  printf '%s;\n\\q\n' "$finish" >&3; exec 3>&-
  wait "$holder"; wait "$contender"
  result="$(<"$race/contender")"
  [[ "$(<"$race/holder")" == "$holder_expected" && "${result:--}" == "$contender_expected" ]] || {
    printf 'unexpected checkpoint/abort statement result: %s\n' "$scenario" >&2; exit 1;
  }
  [[ "$(psql_run -Atc "SELECT COALESCE(terminal_reason, 'active'), checkpoint_revision, checkpoint_offset,
       (terminal_at IS NULL) = ('$terminal' = 'active'),
       checkpoint_generation IS NOT DISTINCT FROM CASE WHEN $rev = 0 THEN NULL ELSE $gen::uuid END,
       CASE WHEN $rev = 0 THEN idle_deadline = $initial_idle ELSE idle_deadline > $initial_idle END,
       EXISTS (SELECT FROM public.ingest_session_cleanup_intents
         WHERE session_id=session.id AND kind='absolute_expiry' AND due_at=session.absolute_deadline),
       (SELECT due_at FROM public.ingest_session_cleanup_intents
         WHERE session_id=session.id AND kind='terminal_reconcile') IS NOT DISTINCT FROM session.terminal_at
     FROM public.ingest_session_reservations AS session WHERE session.id='$sid'")" == "$terminal|$rev|$offset|t|t|t|t|t" ]] || {
    printf 'unexpected committed checkpoint/abort state or cleanup intent: %s\n' "$scenario" >&2; exit 1;
  }
  printf 'ingest session row fence and cleanup intents passed: %s first, %s; checkpoint revision=%s, state=%s\n' "$first" "$finish" "$rev" "$terminal"
done
psql_run -c 'DROP TABLE public.ingest_session_cleanup_intents, public.ingest_session_reservations'
