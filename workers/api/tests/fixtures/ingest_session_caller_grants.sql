-- Exact additional ACLs for upload completion. Catalogue grants stay separate.
GRANT UPDATE (completed_map_id) ON public.ingest_session_reservations TO upload_fixture_writer;
GRANT SELECT (environment_id, library_id, plaintext_sha256, plaintext_bytes,
              object_id, generation, expires_at)
    ON public.storage_digest_reservations TO upload_fixture_writer;
-- PostgreSQL requires UPDATE on at least one column for SELECT FOR UPDATE.
-- The existing reservation trigger rejects changes to this key column.
GRANT UPDATE (environment_id) ON public.storage_digest_reservations TO upload_fixture_writer;
GRANT SELECT (object_id, environment_id, library_id, plaintext_bytes, map_id, page_count)
    ON public.storage_digest_map_intents TO upload_fixture_writer;
GRANT SELECT (expected_hash, map_id, page_index)
    ON public.storage_digest_map_page_intents TO upload_fixture_writer;
GRANT SELECT (object_id, root_version_id, wrapped_record)
    ON public.storage_object_envelope_intents TO upload_fixture_writer;
GRANT SELECT, INSERT ON public.storage_digest_inventory_intents TO upload_fixture_writer;
GRANT SELECT (object_id, root_version_id, quarantined, deleting_at, retain_until),
      INSERT (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id, retain_until)
    ON public.storage_validated_digest_winners TO upload_fixture_writer;
