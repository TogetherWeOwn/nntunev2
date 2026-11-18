-- Run after the successful seeded upgrade, before rolling it back.
DO $$
BEGIN
    IF to_regclass('public.user_cache_grants') IS NOT NULL
        OR to_regclass('public.providers') IS NOT NULL
        OR to_regclass('public.provider_vaults') IS NOT NULL
        OR to_regclass('public.vault_test_leases') IS NOT NULL THEN
        RAISE EXCEPTION 'legacy provider relations survived the purge';
    END IF;
    IF (SELECT count(*) FROM users WHERE id LIKE 'purge-%') <> 4
        OR NOT EXISTS (SELECT 1 FROM users WHERE id = 'purge-marker' AND legacy_vault_established)
        OR (SELECT count(*) FROM users WHERE id LIKE 'purge-%' AND legacy_vault_established) <> 1
        OR NOT EXISTS (SELECT 1 FROM sessions WHERE id = 'purge-revoked' AND revoked)
        OR NOT EXISTS (SELECT 1 FROM webauthn_credentials WHERE cred_id = 'purge-passkey')
        OR NOT EXISTS (SELECT 1 FROM auth_challenges WHERE id = 'purge-challenge')
        OR NOT EXISTS (SELECT 1 FROM cache_objects WHERE content_hash = repeat('0', 64))
        OR NOT EXISTS (SELECT 1 FROM ingest_jobs WHERE id = 'job_purge_fixture' AND status = 'done')
        OR NOT EXISTS (SELECT 1 FROM tracks WHERE id = '11111111-1111-4111-8111-111111111111')
        OR NOT EXISTS (SELECT 1 FROM track_media WHERE source_job_id = 'job_purge_fixture'
                       AND content_hash = repeat('0', 64)) THEN
        RAISE EXCEPTION 'purge modified unrelated account, authentication, ingest, catalogue, or cache state';
    END IF;

    -- Retained media must still enforce its CHECK and both storage-parent
    -- FKs after the neighboring grant constraints have been removed.
    BEGIN
        UPDATE track_media SET bitrate = 0 WHERE source_job_id = 'job_purge_fixture';
        RAISE EXCEPTION 'purged schema accepted non-positive media bitrate';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
    BEGIN
        DELETE FROM cache_objects WHERE content_hash = repeat('0', 64);
        RAISE EXCEPTION 'purged schema allowed deleting a referenced cache object';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        NULL;
    END;
    BEGIN
        DELETE FROM ingest_jobs WHERE id = 'job_purge_fixture';
        RAISE EXCEPTION 'purged schema allowed deleting a referenced ingest job';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        NULL;
    END;
    IF (SELECT count(*) FROM pg_indexes
        WHERE schemaname = 'public' AND tablename = 'track_media'
          AND indexname IN ('idx_track_media_hash', 'idx_track_media_track', 'idx_track_media_job')) <> 3 THEN
        RAISE EXCEPTION 'purge removed retained media access-path indexes';
    END IF;
END $$;
