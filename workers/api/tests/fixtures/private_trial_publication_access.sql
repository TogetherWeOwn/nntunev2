-- Fixture companion ACLs from the overlay and the publication base reads.
GRANT SELECT ON public.storage_s3_locator_intents, public.catalogue_s3_sources,
    public.private_library_admissions TO trial_lock_writer, trial_lock_reader;
GRANT INSERT ON public.storage_s3_locator_intents, public.catalogue_s3_sources,
    public.private_library_admissions TO trial_lock_writer;
GRANT UPDATE (source_job_id, accepted_seq, object_id) ON public.catalogue_s3_sources TO trial_lock_writer;
GRANT UPDATE (accepted_seq, object_id) ON public.private_library_admissions TO trial_lock_writer;

GRANT SELECT (id, deleting_at, role, invite_code_hash, email) ON public.users
    TO trial_lock_writer, trial_lock_reader;
GRANT SELECT (id, kind, issued_by, code_hash, email, used, redeemed_by, redeemed_at, expires_at, revoked_at)
    ON public.invite_codes TO trial_lock_writer, trial_lock_reader;
GRANT SELECT (id, user_id, created_at, expires_at, rotation_counter, revoked)
    ON public.sessions TO trial_lock_writer;
GRANT SELECT (id, user_id, status, accepted_seq) ON public.ingest_jobs TO trial_lock_writer;
GRANT SELECT (id, release_id, medium_no, track_no, mbid, duration_ms),
    INSERT (release_id, medium_no, track_no, mbid, title, duration_ms) ON public.tracks TO trial_lock_writer;
GRANT SELECT (id, release_id, medium_no, track_no, mbid, duration_ms, title) ON public.tracks TO trial_lock_reader;

GRANT SELECT ON public.storage_validated_digest_winners TO trial_lock_writer;
GRANT UPDATE (object_id) ON public.storage_validated_digest_winners TO trial_lock_writer;
GRANT SELECT (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256,
    root_version_id, quarantined, retain_until, delete_until, deleting_at)
    ON public.storage_validated_digest_winners TO trial_lock_reader;
GRANT SELECT (object_id, environment_id, library_id, plaintext_bytes, generation_id,
    provider_configuration_version_id, header_tag) ON public.storage_object_intents TO trial_lock_writer;
GRANT SELECT (object_id, environment_id, library_id, plaintext_bytes, map_id)
    ON public.storage_digest_map_intents TO trial_lock_writer;
GRANT SELECT (map_id, media_version, page_versions) ON public.storage_digest_inventory_intents TO trial_lock_writer;
GRANT SELECT (id, environment_id, owner_id) ON public.storage_library_namespaces TO trial_lock_writer, trial_lock_reader;
GRANT SELECT (environment_id, library_id, reference_id, object_id),
    INSERT (environment_id, library_id, reference_id, object_id)
    ON public.storage_validated_digest_references TO trial_lock_writer;
GRANT EXECUTE ON FUNCTION public.lock_storage_digest_winner(uuid, uuid, bytea, bigint, uuid, uuid, uuid),
    public.attach_storage_digest_reference(uuid, uuid, bytea, bigint, uuid, uuid, uuid, uuid, timestamptz)
    TO trial_lock_writer;
