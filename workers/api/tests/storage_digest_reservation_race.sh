#!/usr/bin/env bash
# Runs only against the migration harness's disposable database.
set -euo pipefail
: "${DATABASE_URL:?scratch database required}"
dir="$(mktemp -d)"
trap 'rm -rf -- "$dir"' EXIT
psql_run() { PGOPTIONS='-c search_path=public,pg_catalog' psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 "$@"; }

psql_run <<'SQL'
INSERT INTO public.storage_environments VALUES
 ('01000000-0000-4000-8000-000000000001'), ('01000000-0000-4000-8000-000000000002');
INSERT INTO public.storage_library_namespaces VALUES
 ('11000000-0000-4000-8000-000000000001','01000000-0000-4000-8000-000000000001',NULL),
 ('11000000-0000-4000-8000-000000000002','01000000-0000-4000-8000-000000000002',NULL);
INSERT INTO public.storage_object_intents
SELECT ('21000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,
       ('31000000-0000-4000-8000-' || lpad(i::text,12,'0'))::uuid,
       CASE WHEN i=9 THEN '01000000-0000-4000-8000-000000000002' ELSE '01000000-0000-4000-8000-000000000001' END::uuid,
       CASE WHEN i=9 THEN '11000000-0000-4000-8000-000000000002' ELSE '11000000-0000-4000-8000-000000000001' END::uuid,
       '41000000-0000-4000-8000-000000000001', 1, 65, decode(repeat('ab',48),'hex')
FROM generate_series(1,9) AS i;
INSERT INTO public.storage_plaintext_digest_claim_intents
SELECT object_id,environment_id,library_id,plaintext_bytes,decode(repeat('ab',32),'hex')
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
 WHERE environment_id='01000000-0000-4000-8000-000000000001' AND object_id<>old_holder LIMIT 1;
 IF public.reserve_storage_digest(next_holder,3600) IS NOT NULL THEN RAISE EXCEPTION 'active holder lost'; END IF;
 IF public.reserve_storage_digest('21000000-0000-4000-8000-000000000099',3600) IS NOT NULL THEN RAISE EXCEPTION 'unknown claim reserved'; END IF;
 IF public.reserve_storage_digest('21000000-0000-4000-8000-000000000009',3600) IS DISTINCT FROM 1 THEN RAISE EXCEPTION 'namespace blocked'; END IF;
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
END $$;
SQL
