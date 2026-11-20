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

psql_run -f "$(dirname -- "${BASH_SOURCE[0]}")/fixtures/storage_digest_reference.sql"
# First test reference-key waits with expired and valid authorization, then both winner lock orders.
for scenario in '11 false ROLLBACK f - 12' '11 false ROLLBACK t - 12' '9 false COMMIT f false' '10 true COMMIT f true' '11 false ROLLBACK t true' '12 true ROLLBACK t false'; do
  read -r id deleting finish expected tombstone other <<<"$scenario"
  mkfifo "$dir/ref-$scenario"; exec 3<>"$dir/ref-$scenario"
  PGAPPNAME=digest_ref_holder psql_run -At <"$dir/ref-$scenario" >"$dir/ref-holder" & holder=$!
  printf "BEGIN; SELECT public.digest_ref_fixture(%s,%s);\n\\! touch '%s/ready-%s'\n" "$id" "$deleting" "$dir" "$scenario" >&3
  for _ in {1..100}; do [[ -e "$dir/ready-$scenario" ]] && break; sleep 0.1; done
  [[ -e "$dir/ready-$scenario" ]] || exit 1
  object="$(printf '21000000-0000-4000-8000-%012d' "$id")"
  deadline=infinity; query="SELECT public.digest_ref_fixture($id, NOT $deleting)"
  if [[ -n "$other" ]]; then
    duration='1 minute'; [[ "$expected" != f ]] || duration='8 seconds'
    deadline="$(psql_run -Atc "SELECT clock_timestamp() + interval '$duration'")"
    query="SELECT public.digest_ref_fixture($other, false, '', '$object', '$deadline')"
  fi
  PGAPPNAME=digest_ref_contender psql_run -Atc "$query" >"$dir/ref-contender" & contender=$!
  blocked=0
  for _ in {1..100}; do
    if [[ "$(psql_run -Atc "SELECT count(*) FROM pg_stat_activity c JOIN pg_stat_activity h ON h.pid=ANY(pg_blocking_pids(c.pid)) WHERE c.application_name='digest_ref_contender' AND h.application_name='digest_ref_holder' AND c.wait_event='transactionid' AND clock_timestamp() < '$deadline'")" == 1 ]]; then blocked=1; break; fi
    sleep 0.1
  done
  [[ "$blocked" == 1 ]] || { printf 'reference contender did not block before deadline: %s\n' "$scenario" >&2; exit 1; }
  if [[ -n "$other" && "$expected" == f ]]; then psql_run -c "SELECT pg_sleep_until('$deadline')" >/dev/null; fi
  printf '%s;\n\\q\n' "$finish" >&3; exec 3>&-
  wait "$holder"; wait "$contender"
  [[ "$(<"$dir/ref-holder")" == t && "$(<"$dir/ref-contender")" == "$expected" ]] || { printf 'unexpected reference race result: %s\n' "$scenario" >&2; exit 1; }
  if [[ -n "$other" ]]; then
    target=none
    if [[ "$expected" == t ]]; then target="$(printf '21000000-0000-4000-8000-%012d' "$other")"; fi
    [[ "$(psql_run -Atc "SELECT coalesce(string_agg(object_id::text, ','), 'none') FROM public.storage_validated_digest_references WHERE reference_id='$object'")" == "$target" ]] || exit 1
    # Remove the verified synthetic reference so the winner-lock cases can reuse the objects.
    psql_run -c "DELETE FROM public.storage_validated_digest_references WHERE reference_id='$object'"
    printf 'reference-key lock case passed: holder rolled back; contender=%s committed reference target=%s\n' "$expected" "$target"
  else
    [[ "$(psql_run -Atc "SELECT (deleting_at IS NOT NULL) = $tombstone AND EXISTS (SELECT FROM public.storage_validated_digest_references WHERE object_id=w.object_id) <> $tombstone FROM public.storage_validated_digest_winners w WHERE object_id='$object'")" == t ]] || exit 1
    printf 'reference/delete lock case passed: delete-first=%s holder=%s contender=%s\n' "$deleting" "$finish" "$expected"
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
DROP FUNCTION public.digest_ref_fixture(int, bool, text, uuid, timestamptz);
SQL
