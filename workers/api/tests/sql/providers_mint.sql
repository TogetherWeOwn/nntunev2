-- Regression test for the providers mint writer
-- (workers/api/src/providers.rs `insert_provider_sql` against migration
-- 0022). Run after ALL migrations are applied (see
-- ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation
-- RAISEs and (with ON_ERROR_STOP) fails the run.
--
-- This test executes the vault-flow mint in its exact shape (literals
-- stand in for the $n parameters): the fenced INSERT with the
-- envelope-version (2) and payload-format (1) literals and no ON
-- CONFLICT. The envelope-version and wrap-nonce CHECKs already have
-- coverage in library_artifacts.sql; this file pins the mint statement
-- itself (fence, insert-per-save, literal constants) plus the columns
-- only the mint binds (payload nonce, wrapped length, label, clock).
-- A trailing PREPARE/EXECUTE section then binds the parameterized
-- shape through a typed prepared statement (the production
-- tokio-postgres path: the id binds as text and casts server-side),
-- and two sections pin the rotation prune and the vault-delete purge
-- (unreferenced rows go, grant-referenced rows stay).

BEGIN;

DO $$
DECLARE
    row_count_var INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-pm-1', 'pm-1@example.com', 1700000000),
           ('user-pm-2', 'pm-2@example.com', 1700000000);

    -- Statement: the fenced mint. Envelope version and payload format
    -- are SQL literals (2, 1), never caller parameters; there is no ON
    -- CONFLICT (each save mints a fresh credential identity).
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'user-pm-1', 'news.example.com',
           '\x0102'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000000, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    SELECT count(*) INTO row_count_var FROM providers
    WHERE id = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa'
      AND creds_envelope_version = 2 AND creds_payload_format = 1
      AND creds_dek_wrapped_nonce IS NOT NULL AND creds_dek_wrapped IS NOT NULL
      AND creds_payload_nonce IS NOT NULL AND encrypted_creds IS NOT NULL;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'providers mint did not land one v2 row';
    END IF;

    -- Insert-per-save: a second mint for the same user lands a second
    -- row (a re-save mints a fresh credential identity; the pool
    -- resolves the latest row per user).
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT 'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb', 'user-pm-1', 'news.example.com',
           '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000001, 1700000001
    FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    IF (SELECT count(*) FROM providers WHERE user_id = 'user-pm-1') <> 2 THEN
        RAISE EXCEPTION 'second providers mint did not land a second row';
    END IF;

    -- No silent convergence: without ON CONFLICT a duplicate id raises
    -- 23505 instead of landing on the first row (a UUID collision fails
    -- closed; the vault PUT fails and the client retries with a fresh
    -- mint).
    BEGIN
        INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, last_verified_at)
        SELECT 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 'user-pm-1', 'news.example.com',
               '\x0506'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1,
               1700000002, 1700000002
        FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
        RAISE EXCEPTION 'mint converged a duplicate id onto the first row';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- A short payload nonce is rejected (the wrap-nonce CHECK has its
    -- own coverage in library_artifacts.sql; this is the other 12-byte
    -- column the mint binds).
    BEGIN
        INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, last_verified_at)
        SELECT 'cccccccc-cccc-4ccc-8ccc-cccccccccccc', 'user-pm-1', 'news.example.com',
               '\x0102'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x00'::bytea,
               1,
               1700000000, 1700000000
        FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
        RAISE EXCEPTION 'CHECK accepted a short payload nonce';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- The wrapped DEK is exactly 48 bytes (32-byte DEK + 16-byte tag);
    -- anything else is rejected.
    BEGIN
        INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, last_verified_at)
        SELECT 'dddddddd-dddd-4ddd-8ddd-dddddddddddd', 'user-pm-1', 'news.example.com',
               '\x0102'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1,
               1700000000, 1700000000
        FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
        RAISE EXCEPTION 'CHECK accepted a short wrapped DEK';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Labels longer than 64 chars are rejected (the minter truncates at
    -- a char boundary before binding).
    BEGIN
        INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, last_verified_at)
        SELECT 'eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee', 'user-pm-1', repeat('l', 65),
               '\x0102'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1,
               1700000000, 1700000000
        FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
        RAISE EXCEPTION 'CHECK accepted a label longer than 64 chars';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- The mint clock is bounded like every other epoch column.
    BEGIN
        INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
             creds_payload_format, created_at, last_verified_at)
        SELECT 'ffffffff-ffff-4fff-8fff-ffffffffffff', 'user-pm-1', 'news.example.com',
               '\x0102'::bytea,
               2, 1,
               '\x000000000000000000000000'::bytea,
               '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
               '\x000000000000000000000000'::bytea,
               1,
               253402300800, 253402300800
        FROM (SELECT id FROM users WHERE id = 'user-pm-1' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
        RAISE EXCEPTION 'CHECK accepted a mint clock past the century bound';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Deletion fence (ADR 0002 rule 3): a mint for a deleting account
    -- lands zero rows without raising.
    UPDATE users SET deleting_at = NOW() WHERE id = 'user-pm-2';
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '11111111-2222-4333-8444-555555555555', 'user-pm-2', 'news.example.com',
           '\x0102'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000000, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-pm-2' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    IF (SELECT count(*) FROM providers WHERE user_id = 'user-pm-2') <> 0 THEN
        RAISE EXCEPTION 'fenced providers mint landed a row behind the deletion marker';
    END IF;

    -- A mint for a missing user produces zero rows through the fence
    -- (no row, so no FK violation either): the fence decides, not the
    -- constraint.
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '66666666-7777-4888-8999-000000000000', 'user-pm-gone', 'news.example.com',
           '\x0102'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000000, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-pm-gone' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    IF (SELECT count(*) FROM providers WHERE user_id = 'user-pm-gone') <> 0 THEN
        RAISE EXCEPTION 'providers mint for a missing user produced a row';
    END IF;

    -- RESTRICT (ADR 0002 rule 3): historical grants keep pointing at the
    -- row whose credentials fetched them — a provider row with grants
    -- cannot be deleted. ON DELETE RESTRICT raises restrict_violation
    -- (23001), so catch both spellings.
    INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
    VALUES ('aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            1992294, 'nntunev2-audio-staging', 'audio/aa/aa/aaaa', 1700000000);
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    VALUES ('user-pm-1',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa', 1700000000);
    BEGIN
        DELETE FROM providers WHERE id = 'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa';
        RAISE EXCEPTION 'provider delete cascaded past RESTRICT grants';
    EXCEPTION WHEN foreign_key_violation OR restrict_violation THEN
        NULL;
    END;
END $$;

-- Typed bind (the production tokio-postgres path): the minter binds
-- the id as a Rust String (text), and the statement casts server-side
-- ($1::text::uuid). PREPARE the exact parameterized shape of
-- insert_provider_sql() and EXECUTE it with a text id: without the
-- cast the bind fails before the statement runs. Top-level (not in a
-- DO block) so the prepared statement binds exactly like the
-- driver's. Keep in sync with insert_provider_sql(); the
-- mint_sql_shape_is_pinned unit test pins that side.
INSERT INTO users (id, email, created_at)
VALUES ('user-pm-3', 'pm-3@example.com', 1700000000);

PREPARE providers_mint_typed(text, text, text, bytea, integer, bytea, bytea, bytea, bigint, bigint) AS
INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
     creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
     creds_payload_format, created_at, last_verified_at)
SELECT $1::text::uuid, $2, $3, $4, 2, $5, $6, $7, $8, 1, $9, $10
FROM (SELECT id FROM users WHERE id = $2 AND deleting_at IS NULL FOR SHARE) AS writer_fence;

EXECUTE providers_mint_typed('33333333-3333-4333-8333-333333333333', 'user-pm-3', 'news.example.com',
       '\x0102'::bytea, 1,
       '\x000000000000000000000000'::bytea,
       '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
       '\x000000000000000000000000'::bytea,
       1700000000, 1700000000);

DEALLOCATE providers_mint_typed;

DO $$
BEGIN
    IF (SELECT count(*) FROM providers
        WHERE id = '33333333-3333-4333-8333-333333333333'
          AND user_id = 'user-pm-3') <> 1 THEN
        RAISE EXCEPTION 'typed-bind mint did not land one row';
    END IF;
END $$;

-- Rotation prune (mirror of prune_superseded_providers_sql): after a
-- save mints the keep-id, the user's older rows go unless a grant
-- references them. Vault-delete purge (mirror of
-- purge_unreferenced_providers_sql): every unreferenced row goes.
DO $$
DECLARE
    row_count_var INTEGER;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('user-pm-4', 'pm-4@example.com', 1700000000),
           ('user-pm-5', 'pm-5@example.com', 1700000000);

    -- Two live rows for user-pm-4; a grant references the older one
    -- (reusing the cache object seeded above).
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '44444444-4444-4444-8444-444444444444', 'user-pm-4', 'news.example.com',
           '\x0102'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000000, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-pm-4' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '55555555-5555-4555-8555-555555555555', 'user-pm-4', 'news.example.com',
           '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000001, 1700000001
    FROM (SELECT id FROM users WHERE id = 'user-pm-4' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    VALUES ('user-pm-4',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            '44444444-4444-4444-8444-444444444444', 1700000000);

    -- Prune with the fresh row as the keep-id: the referenced older
    -- row AND the keep-id survive.
    DELETE FROM providers
    WHERE user_id = 'user-pm-4' AND id <> '55555555-5555-4555-8555-555555555555'
      AND NOT EXISTS (SELECT 1 FROM user_cache_grants g
                      WHERE g.user_id = 'user-pm-4' AND g.provider_id = providers.id);
    SELECT count(*) INTO row_count_var FROM providers WHERE user_id = 'user-pm-4';
    IF row_count_var <> 2 THEN
        RAISE EXCEPTION 'prune removed a referenced or kept row';
    END IF;

    -- The grant drains: the same prune now removes the superseded row
    -- and keeps only the fresh mint.
    DELETE FROM user_cache_grants
    WHERE user_id = 'user-pm-4' AND provider_id = '44444444-4444-4444-8444-444444444444';
    DELETE FROM providers
    WHERE user_id = 'user-pm-4' AND id <> '55555555-5555-4555-8555-555555555555'
      AND NOT EXISTS (SELECT 1 FROM user_cache_grants g
                      WHERE g.user_id = 'user-pm-4' AND g.provider_id = providers.id);
    SELECT count(*) INTO row_count_var FROM providers WHERE user_id = 'user-pm-4';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'prune kept an unreferenced superseded row';
    END IF;
    IF (SELECT count(*) FROM providers
        WHERE id = '55555555-5555-4555-8555-555555555555') <> 1 THEN
        RAISE EXCEPTION 'prune removed the kept mint';
    END IF;

    -- Purge: one referenced and one unreferenced row for user-pm-5;
    -- the purge takes only the unreferenced one.
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '66666666-6666-4666-8666-666666666666', 'user-pm-5', 'news.example.com',
           '\x0102'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000000, 1700000000
    FROM (SELECT id FROM users WHERE id = 'user-pm-5' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    INSERT INTO providers (id, user_id, label, encrypted_creds, creds_envelope_version,
         creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
         creds_payload_format, created_at, last_verified_at)
    SELECT '77777777-7777-4777-8777-777777777777', 'user-pm-5', 'news.example.com',
           '\x0304'::bytea,
           2, 1,
           '\x000000000000000000000000'::bytea,
           '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
           '\x000000000000000000000000'::bytea,
           1,
           1700000001, 1700000001
    FROM (SELECT id FROM users WHERE id = 'user-pm-5' AND deleting_at IS NULL FOR SHARE) AS writer_fence;
    INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
    VALUES ('user-pm-5',
            'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            '66666666-6666-4666-8666-666666666666', 1700000000);
    DELETE FROM providers
    WHERE user_id = 'user-pm-5'
      AND NOT EXISTS (SELECT 1 FROM user_cache_grants g
                      WHERE g.user_id = 'user-pm-5' AND g.provider_id = providers.id);
    SELECT count(*) INTO row_count_var FROM providers WHERE user_id = 'user-pm-5';
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'purge took the wrong rows';
    END IF;
    IF (SELECT count(*) FROM providers
        WHERE id = '66666666-6666-4666-8666-666666666666') <> 1 THEN
        RAISE EXCEPTION 'purge removed a grant-referenced row';
    END IF;
END $$;

ROLLBACK;
