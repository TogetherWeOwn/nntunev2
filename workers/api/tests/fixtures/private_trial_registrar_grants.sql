-- Disposable registrar. Lock permission is limited to the job's identity column.
CREATE ROLE upload_fixture_registrar NOLOGIN NOINHERIT;
GRANT SELECT (id, user_id, status, accepted_seq), UPDATE (accepted_seq)
    ON public.ingest_jobs TO upload_fixture_registrar;
GRANT SELECT (id, environment_id, owner_id) ON public.storage_library_namespaces TO upload_fixture_registrar;
GRANT SELECT (id, absolute_deadline), INSERT (id, library_id, principal_id, idempotency_key, fingerprint, absolute_deadline, idle_deadline)
    ON public.ingest_session_reservations TO upload_fixture_registrar;
GRANT INSERT (session_id, kind, due_at) ON public.ingest_session_cleanup_intents TO upload_fixture_registrar;
GRANT INSERT, SELECT ON public.storage_object_intents, public.storage_plaintext_digest_claim_intents
    TO upload_fixture_registrar;
GRANT SELECT, INSERT, UPDATE (object_id, generation, expires_at)
    ON public.storage_digest_reservations TO upload_fixture_registrar;
GRANT INSERT ON public.storage_digest_map_intents, public.storage_digest_map_page_intents,
    public.storage_object_envelope_intents TO upload_fixture_registrar;
GRANT EXECUTE ON FUNCTION public.reserve_storage_digest(UUID, INTEGER),
    public.store_storage_digest_candidate(UUID, UUID, UUID, UUID, BIGINT, BYTEA[])
    TO upload_fixture_registrar;
