-- Exercise the application statement. Directly seeded winners model existing
-- rows only; this fixture does not exercise or authorize verified publication.
BEGIN ISOLATION LEVEL READ COMMITTED;
CREATE FUNCTION pg_temp.claim_lookup(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID)
RETURNS TABLE (object_id UUID, generation_id UUID, provider_configuration_version_id UUID,
               map_id UUID, root_version_id UUID, media_version TEXT, page_versions TEXT[])
LANGUAGE sql VOLATILE AS :'storage_digest_claim_lookup_sql';

CREATE TEMP TABLE digest_lookup_fixture AS
SELECT n, ('ca000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid AS object_id,
          ('da000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid AS generation_id,
          ('fa000000-0000-4000-8000-' || lpad(n::text, 12, '0'))::uuid AS map_id,
          'aa000000-0000-4000-8000-000000000001'::uuid AS environment_id,
          'ba000000-0000-4000-8000-000000000001'::uuid AS library_id,
          'ea000000-0000-4000-8000-000000000001'::uuid AS provider_id,
          'ea000000-0000-4000-8000-000000000002'::uuid AS root_id,
          CASE WHEN n=1 THEN 16777217 ELSE 1 END::bigint AS bytes,
          decode(lpad(to_hex(n), 64, '0'), 'hex') AS sha
FROM generate_series(1, 8) AS n;
INSERT INTO public.storage_environments SELECT DISTINCT environment_id FROM digest_lookup_fixture;
INSERT INTO public.storage_library_namespaces SELECT DISTINCT library_id, environment_id, NULL::text FROM digest_lookup_fixture;
INSERT INTO public.storage_object_intents
SELECT object_id, generation_id, environment_id, library_id, provider_id, bytes,
       48 + bytes + 16 * ((bytes + 65535) / 65536), decode(repeat('ab', 48), 'hex') FROM digest_lookup_fixture;
INSERT INTO public.storage_plaintext_digest_claim_intents
SELECT object_id, environment_id, library_id, bytes, sha FROM digest_lookup_fixture;
INSERT INTO public.storage_digest_map_intents (map_id, object_id, environment_id, library_id, plaintext_bytes)
SELECT map_id, object_id, environment_id, library_id, bytes FROM digest_lookup_fixture;
INSERT INTO public.storage_digest_map_page_intents (map_id, chunk_count, page_index, expected_hash)
SELECT map_id, chunk_count, page, decode(repeat('cd', 32), 'hex')
FROM public.storage_digest_map_intents JOIN digest_lookup_fixture USING (map_id)
CROSS JOIN LATERAL generate_series(0, page_count - 1) AS page;
INSERT INTO public.storage_digest_inventory_intents
SELECT map_id, CASE WHEN n<>4 THEN 'media-pin' END,
       CASE WHEN n=1 THEN ARRAY['z-first','a-second'] WHEN n=4 THEN ARRAY[NULL::text] ELSE ARRAY['page-pin'] END
FROM digest_lookup_fixture WHERE n<>3;
INSERT INTO public.storage_object_envelope_intents
SELECT i.object_id, f.root_id, decode('4e4e544d454430310002','hex') || uuid_send(f.root_id)
       || uuid_send(i.environment_id) || uuid_send(i.library_id) || uuid_send(i.object_id)
       || uuid_send(i.generation_id) || uuid_send(i.provider_configuration_version_id)
       || int8send(i.plaintext_bytes) || int8send(i.envelope_bytes) || i.header_tag || decode(repeat('00',60),'hex')
FROM public.storage_object_intents i JOIN digest_lookup_fixture f USING (object_id);
INSERT INTO public.storage_validated_digest_winners
    (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id,
     retain_until, quarantined, deleting_at, delete_until)
SELECT object_id, environment_id, library_id, bytes, sha, root_id, '-infinity', n=5,
       CASE WHEN n=6 THEN clock_timestamp() END, CASE WHEN n=7 THEN 'infinity' ELSE '-infinity' END::timestamptz
FROM digest_lookup_fixture WHERE n<>2;

DO $$
DECLARE
    f digest_lookup_fixture; other digest_lookup_fixture; fault text; pin record;
    claim_ref CONSTANT uuid := 'ab000000-0000-4000-8000-000000000001';
    other_ref CONSTANT uuid := 'ab000000-0000-4000-8000-000000000002';
BEGIN
    FOR f IN SELECT * FROM digest_lookup_fixture WHERE n BETWEEN 2 AND 7 LOOP
        IF EXISTS (SELECT FROM pg_temp.claim_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
            f.object_id, f.generation_id, f.root_id, claim_ref, 'infinity', f.provider_id)) THEN
            RAISE EXCEPTION 'refused row returned a binding: %', f.n;
        END IF;
    END LOOP;
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    FOREACH fault IN ARRAY ARRAY['environment','library','digest','size','object','generation','root','provider','expired'] LOOP
        IF EXISTS (SELECT FROM pg_temp.claim_lookup(
            CASE WHEN fault='environment' THEN gen_random_uuid() ELSE f.environment_id END,
            CASE WHEN fault='library' THEN gen_random_uuid() ELSE f.library_id END,
            CASE WHEN fault='digest' THEN decode(repeat('00',32),'hex') ELSE f.sha END,
            f.bytes + (fault='size')::int,
            CASE WHEN fault='object' THEN gen_random_uuid() ELSE f.object_id END,
            CASE WHEN fault='generation' THEN gen_random_uuid() ELSE f.generation_id END,
            CASE WHEN fault='root' THEN gen_random_uuid() ELSE f.root_id END, claim_ref,
            CASE WHEN fault='expired' THEN '-infinity' ELSE 'infinity' END::timestamptz,
            CASE WHEN fault='provider' THEN gen_random_uuid() ELSE f.provider_id END)) THEN
            RAISE EXCEPTION 'mismatched % returned a binding', fault;
        END IF;
    END LOOP;
    IF EXISTS (SELECT FROM public.storage_validated_digest_references WHERE environment_id=f.environment_id) THEN
        RAISE EXCEPTION 'refusal wrote a reference';
    END IF;
    SELECT * INTO STRICT pin FROM pg_temp.claim_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
        f.object_id, f.generation_id, f.root_id, claim_ref, 'infinity', f.provider_id);
    IF (pin.object_id, pin.generation_id, pin.provider_configuration_version_id, pin.map_id,
        pin.root_version_id, pin.media_version, pin.page_versions) IS DISTINCT FROM
       (f.object_id, f.generation_id, f.provider_id, f.map_id, f.root_id, 'media-pin'::text, ARRAY['z-first','a-second'])
       OR NOT EXISTS (SELECT FROM public.storage_validated_digest_references
           WHERE (environment_id, library_id, reference_id, object_id) = (f.environment_id,f.library_id,claim_ref,f.object_id)) THEN
        RAISE EXCEPTION 'binding mismatch or missing exact reference attachment';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.claim_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
        f.object_id, f.generation_id, f.root_id, claim_ref, 'infinity', f.provider_id)) THEN
        RAISE EXCEPTION 'duplicate attachment returned a binding';
    END IF;
    SELECT * INTO STRICT other FROM digest_lookup_fixture WHERE n=8;
    IF EXISTS (SELECT FROM pg_temp.claim_lookup(other.environment_id, other.library_id, other.sha, other.bytes,
        other.object_id, other.generation_id, other.root_id, claim_ref, 'infinity', other.provider_id)) THEN
        RAISE EXCEPTION 'reference held by another winner returned a binding';
    END IF;
    IF (SELECT count(*) FROM public.storage_validated_digest_references
        WHERE (environment_id, library_id) = (f.environment_id, f.library_id)) <> 1
       OR NOT EXISTS (SELECT FROM public.storage_validated_digest_references
           WHERE (environment_id, library_id, reference_id, object_id) = (f.environment_id,f.library_id,claim_ref,f.object_id)) THEN
        RAISE EXCEPTION 'reference conflict wrote or reassigned an attachment';
    END IF;
    -- The second winner is eligible; only the occupied reference blocked it.
    PERFORM FROM pg_temp.claim_lookup(other.environment_id, other.library_id, other.sha, other.bytes,
        other.object_id, other.generation_id, other.root_id, other_ref, 'infinity', other.provider_id);
    IF NOT FOUND THEN
        RAISE EXCEPTION 'second winner refused an unoccupied reference';
    END IF;
END $$;

CREATE FUNCTION pg_temp.read_lookup(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ, UUID, TIMESTAMPTZ)
RETURNS TABLE (object_id UUID, generation_id UUID, provider_configuration_version_id UUID,
               map_id UUID, root_version_id UUID, media_version TEXT, page_versions TEXT[], usable_until TIMESTAMPTZ)
LANGUAGE sql VOLATILE AS :'storage_digest_read_lookup_sql';

DO $$
DECLARE
    f digest_lookup_fixture; pin record; fault text;
    ref CONSTANT uuid := 'ab000000-0000-4000-8000-000000000001';
    cutoff CONSTANT timestamptz := clock_timestamp() + interval '5 minutes';
BEGIN
    -- Give each otherwise ineligible winner an existing reference to isolate its refusal.
    INSERT INTO public.storage_validated_digest_references
    SELECT environment_id, library_id, object_id, object_id FROM digest_lookup_fixture WHERE n BETWEEN 3 AND 7;
    FOR f IN SELECT * FROM digest_lookup_fixture WHERE n BETWEEN 2 AND 8 LOOP
        IF EXISTS (SELECT FROM pg_temp.read_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
            f.object_id, f.generation_id, f.root_id, CASE WHEN f.n=8 THEN ref ELSE f.object_id END,
            cutoff, f.provider_id, cutoff)) THEN
            RAISE EXCEPTION 'ineligible playback row returned a binding: %', f.n;
        END IF;
    END LOOP;
    SELECT * INTO STRICT f FROM digest_lookup_fixture WHERE n=1;
    FOREACH fault IN ARRAY ARRAY['environment','library','digest','size','object','generation','root',
        'reference','provider','expired','beyond authorization','unbounded','unbounded authorization'] LOOP
        IF EXISTS (SELECT FROM pg_temp.read_lookup(
            CASE WHEN fault='environment' THEN gen_random_uuid() ELSE f.environment_id END,
            CASE WHEN fault='library' THEN gen_random_uuid() ELSE f.library_id END,
            CASE WHEN fault='digest' THEN decode(repeat('00',32),'hex') ELSE f.sha END,
            f.bytes + (fault='size')::int,
            CASE WHEN fault='object' THEN gen_random_uuid() ELSE f.object_id END,
            CASE WHEN fault='generation' THEN gen_random_uuid() ELSE f.generation_id END,
            CASE WHEN fault='root' THEN gen_random_uuid() ELSE f.root_id END,
            CASE WHEN fault='reference' THEN gen_random_uuid() ELSE ref END,
            CASE WHEN fault IN ('unbounded', 'unbounded authorization') THEN 'infinity' ELSE cutoff END,
            CASE WHEN fault='provider' THEN gen_random_uuid() ELSE f.provider_id END,
            CASE fault WHEN 'expired' THEN cutoff - interval '6 minutes' WHEN 'unbounded' THEN 'infinity'
                       WHEN 'beyond authorization' THEN cutoff + interval '1 second' ELSE cutoff END)) THEN
            RAISE EXCEPTION 'playback accepted %', fault;
        END IF;
    END LOOP;
    IF EXISTS (SELECT FROM public.storage_validated_digest_winners
        WHERE environment_id=f.environment_id AND read_until <> '-infinity')
       OR (SELECT count(*) FROM public.storage_validated_digest_references WHERE environment_id=f.environment_id) <> 7 THEN
        RAISE EXCEPTION 'refused playback changed leases or references';
    END IF;
    SELECT * INTO STRICT pin FROM pg_temp.read_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
        f.object_id, f.generation_id, f.root_id, ref, cutoff, f.provider_id, cutoff);
    IF (pin.object_id, pin.generation_id, pin.provider_configuration_version_id, pin.map_id,
        pin.root_version_id, pin.media_version, pin.page_versions, pin.usable_until) IS DISTINCT FROM
       (f.object_id, f.generation_id, f.provider_id, f.map_id, f.root_id, 'media-pin'::text, ARRAY['z-first','a-second'], cutoff) THEN
        RAISE EXCEPTION 'wrong playback binding';
    END IF;
    SELECT * INTO STRICT pin FROM pg_temp.read_lookup(f.environment_id, f.library_id, f.sha, f.bytes,
        f.object_id, f.generation_id, f.root_id, ref, cutoff, f.provider_id, cutoff - interval '1 minute');
    IF pin.usable_until IS DISTINCT FROM cutoff - interval '1 minute'
       OR (SELECT read_until FROM public.storage_validated_digest_winners WHERE object_id=f.object_id)
        IS DISTINCT FROM cutoff THEN
        RAISE EXCEPTION 'second read shortened the existing protection';
    END IF;
    DELETE FROM public.storage_validated_digest_references WHERE object_id=f.object_id;
    IF NOT FOUND OR public.admit_storage_digest_delete(f.environment_id, f.library_id, f.sha, f.bytes,
        f.object_id, f.generation_id, f.root_id) IS DISTINCT FROM false THEN
        RAISE EXCEPTION 'delete bypassed live read protection after last reference removal';
    END IF;
END $$;
-- Require the runner's pinned disposable connection for session fixtures.
\if :{?hosted_ingest_session_fixture}
\else
\echo 'skip: ingest session fixtures require the hosted disposable database'
ROLLBACK;
\quit
\endif
\ir ../fixtures/ingest_session_reserve.sql
\ir ../fixtures/ingest_session_terminate.sql
\ir ../fixtures/ingest_session_advance_checkpoint.sql
\ir ../fixtures/ingest_session_status.sql
\ir ../fixtures/ingest_session_cleanup_delivery.sql
ROLLBACK;
