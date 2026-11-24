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
