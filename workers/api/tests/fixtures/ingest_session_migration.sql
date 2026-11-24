-- Run on the two throwaway databases after the tracked candidate apply.
DO $$
BEGIN
    IF EXISTS (
        SELECT FROM pg_class c CROSS JOIN LATERAL aclexplode(c.relacl) a
        WHERE c.oid IN ('public.ingest_session_reservations'::regclass,
                        'public.ingest_session_cleanup_intents'::regclass)
          AND a.grantee <> c.relowner
    ) THEN
        RAISE EXCEPTION 'upload tables inherited default privileges';
    END IF;
END $$;

-- The dedicated role is provisioned only inside the fixture. It represents a
-- trusted worker using the environment/library/principal checks in its queries.
GRANT SELECT (id, environment_id) ON public.storage_library_namespaces TO upload_fixture_writer;
GRANT SELECT (object_id, generation_id, environment_id, library_id, provider_configuration_version_id,
              plaintext_bytes, header_tag) ON public.storage_object_intents TO upload_fixture_writer;
GRANT SELECT ON public.ingest_session_reservations, public.ingest_session_cleanup_intents
    TO upload_fixture_writer;
GRANT INSERT (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline),
      UPDATE (idempotency_key, idle_deadline, checkpoint_generation, checkpoint_revision, checkpoint_offset,
              write_lease_id, write_lease_generation, terminal_reason, terminal_at,
              multipart_object_id, multipart_bucket, multipart_upload_id)
    ON public.ingest_session_reservations TO upload_fixture_writer;
GRANT INSERT (session_id, kind, due_at),
      UPDATE (delivery_claim_id, claimed_until, enqueued_at)
    ON public.ingest_session_cleanup_intents TO upload_fixture_writer;

SET ROLE upload_fixture_writer;
CREATE TEMP TABLE upload_claims (session_id text, kind text, claim_id text, PRIMARY KEY (session_id, kind));
-- worker statements

BEGIN ISOLATION LEVEL READ COMMITTED;
DO $$
DECLARE
    env CONSTANT uuid := '10000000-0000-4000-8000-000000000001';
    lib CONSTANT uuid := '20000000-0000-4000-8000-000000000001';
    principal CONSTANT text := 'upload-owner';
    sid CONSTANT uuid := '30000000-0000-4000-8000-000000000001';
    lost CONSTANT uuid := '30000000-0000-4000-8000-000000000002';
    object_id CONSTANT text := '40000000-0000-4000-8000-000000000001';
    generation CONSTANT uuid := '50000000-0000-4000-8000-000000000001';
    provider CONSTANT text := '60000000-0000-4000-8000-000000000001';
    lease CONSTANT uuid := gen_random_uuid();
    fp CONSTANT bytea := decode(repeat('ab', 32), 'hex');
BEGIN
    IF has_schema_privilege(current_user, 'public', 'CREATE')
       OR has_table_privilege(current_user, 'public.ingest_session_reservations', 'DELETE,TRUNCATE')
       OR has_table_privilege(current_user, 'public.ingest_session_cleanup_intents', 'DELETE,TRUNCATE')
       OR has_any_column_privilege(current_user, 'public.users', 'SELECT,UPDATE')
       OR has_column_privilege(current_user, 'public.ingest_session_reservations', 'completed_map_id', 'UPDATE') THEN
        RAISE EXCEPTION 'upload writer has authority outside the session and cleanup calls';
    END IF;
    IF (SELECT pg_temp.reserve(env, lib, principal, 'upload-key', fp, sid)) IS DISTINCT FROM sid THEN
        RAISE EXCEPTION 'initial reservation differs';
    END IF;
    IF (SELECT pg_temp.reserve(env, lib, principal, 'upload-key', fp, gen_random_uuid())) IS DISTINCT FROM sid
       OR EXISTS (SELECT FROM pg_temp.status(env, lib, 'wrong-owner', sid))
       OR EXISTS (SELECT FROM pg_temp.status(gen_random_uuid(), lib, principal, sid))
       OR EXISTS (SELECT FROM pg_temp.status(env, gen_random_uuid(), principal, sid))
       OR (SELECT state FROM pg_temp.status(env, lib, principal, sid)) IS DISTINCT FROM 'active' THEN
        RAISE EXCEPTION 'reservation replay or scoped status differs';
    END IF;
    IF (SELECT pg_temp.acquire_write_lease(env, lib, principal, sid, fp, generation, 0, 0, lease))
            IS DISTINCT FROM lease THEN
        RAISE EXCEPTION 'initial lease differs';
    END IF;
    IF (SELECT pg_temp.advance_checkpoint(env, lib, principal, sid, fp, generation, 0, 0, 64, lease))
            IS DISTINCT FROM 1 THEN
        RAISE EXCEPTION 'initial checkpoint differs';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.advance_checkpoint(env, lib, principal, sid, fp, generation, 0, 0, 64, lease)) THEN
        RAISE EXCEPTION 'consumed lease accepted';
    END IF;
    IF (SELECT pg_temp.terminate(env, lib, principal, sid, 'abort', NULL, NULL, NULL, NULL, NULL))
            IS DISTINCT FROM 'aborted' THEN
        RAISE EXCEPTION 'terminal transition differs';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.acquire_write_lease(env, lib, principal, sid, fp, generation, 1, 64, lease)) THEN
        RAISE EXCEPTION 'tombstone accepted a write lease';
    END IF;
    INSERT INTO upload_claims
    SELECT session_id, kind, delivery_claim_id FROM pg_temp.claim_cleanup(env::text, lib::text, principal, sid::text)
    WHERE kind = 'terminal_reconcile';

    IF (SELECT pg_temp.reserve(env, lib, principal, 'lost-upload-key', fp, lost)) IS DISTINCT FROM lost THEN
        RAISE EXCEPTION 'lost-initiation fixture reservation failed';
    END IF;
    IF (SELECT pg_temp.acquire_write_lease(env, lib, principal, lost, fp, generation, 0, 0, lease))
            IS DISTINCT FROM lease THEN
        RAISE EXCEPTION 'lost-initiation fixture lease failed';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_temp.lock_upload(env::text, lib::text, principal, lost::text, fp,
            generation::text, 0, 0, lease::text, object_id, provider, 'fixture-bucket',
            64, decode(repeat('ab', 48), 'hex')) WHERE NOT claimed AND upload_id IS NULL) THEN
        RAISE EXCEPTION 'scoped multipart handoff could not lock the object';
    END IF;
    -- Model a claimed object with an unrecorded provider ID and elapsed idle deadline.
    UPDATE public.ingest_session_reservations
    SET multipart_object_id = object_id::uuid, multipart_bucket = 'fixture-bucket',
        idle_deadline = clock_timestamp() - interval '1 second'
    WHERE id = lost;
    IF (SELECT pg_temp.terminate(env, lib, principal, lost, 'expire', NULL, NULL, NULL, NULL, NULL))
            IS DISTINCT FROM 'expired' THEN
        RAISE EXCEPTION 'lost-initiation fixture expiry failed';
    END IF;
    INSERT INTO upload_claims
    SELECT session_id, kind, delivery_claim_id FROM pg_temp.claim_cleanup(env::text, lib::text, principal, lost::text);
    IF (SELECT count(*) FROM upload_claims WHERE session_id = lost::text) <> 2 THEN
        RAISE EXCEPTION 'expected one expiry claim and one terminal reconcile claim';
    END IF;
END $$;
COMMIT;

BEGIN ISOLATION LEVEL READ COMMITTED;
DO $$
DECLARE
    env CONSTANT text := '10000000-0000-4000-8000-000000000001';
    lib CONSTANT text := '20000000-0000-4000-8000-000000000001';
    sid CONSTANT text := '30000000-0000-4000-8000-000000000002';
    unclaimed CONSTANT text := '30000000-0000-4000-8000-000000000001';
    provider CONSTANT text := '60000000-0000-4000-8000-000000000001';
    claim text;
BEGIN
    SELECT claim_id INTO STRICT claim FROM upload_claims WHERE session_id = unclaimed AND kind = 'terminal_reconcile';
    IF NOT EXISTS (SELECT FROM pg_temp.reconcile(env, lib, 'upload-owner', unclaimed, claim,
                   'fixture-bucket', provider, NULL) WHERE media_key IS NULL AND upload_id IS NULL) THEN
        RAISE EXCEPTION 'unclaimed session could not be reconciled';
    END IF;
    SELECT claim_id INTO STRICT claim FROM upload_claims WHERE session_id = sid AND kind = 'terminal_reconcile';
    IF NOT EXISTS (SELECT FROM pg_temp.reconcile(env, lib, 'upload-owner', sid, claim,
                   'fixture-bucket', provider, 'recovered-fixture-id')
                   WHERE upload_id = 'recovered-fixture-id' AND media_key =
                         'media/40000000000040008000000000000001/50000000000040008000000000000001') THEN
        RAISE EXCEPTION 'committed cleanup claim could not pin the recovered upload ID';
    END IF;
END $$;
COMMIT;

BEGIN ISOLATION LEVEL READ COMMITTED;
DO $$
DECLARE
    env CONSTANT text := '10000000-0000-4000-8000-000000000001';
    lib CONSTANT text := '20000000-0000-4000-8000-000000000001';
    sid CONSTANT text := '30000000-0000-4000-8000-000000000002';
    claim text;
BEGIN
    SELECT claim_id INTO STRICT claim FROM upload_claims WHERE session_id = sid AND kind = 'terminal_reconcile';
    IF (SELECT multipart_upload_id FROM public.ingest_session_reservations WHERE id = sid::uuid)
            IS DISTINCT FROM 'recovered-fixture-id' THEN
        RAISE EXCEPTION 'recovered upload ID did not survive commit';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(env, lib, 'wrong-owner', sid, 'terminal_reconcile', claim)) THEN
        RAISE EXCEPTION 'wrong owner acknowledged the claim';
    END IF;
    IF NOT EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(env, lib, 'upload-owner', sid, 'terminal_reconcile', claim)) THEN
        RAISE EXCEPTION 'scoped acknowledgement failed';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.ack_cleanup_enqueue(env, lib, 'upload-owner', sid, 'terminal_reconcile', claim)) THEN
        RAISE EXCEPTION 'claim was acknowledged twice';
    END IF;
END $$;
COMMIT;
