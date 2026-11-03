-- Regression test for the posted-file record (migrations 0027 + 0028,
-- workers/ingest/src/posted_files.rs). Run after ALL migrations are applied
-- (see ../run_migration_tests.sh). Everything happens inside one transaction
-- that is rolled back, so it leaves no rows; any broken expectation RAISEs
-- and (with ON_ERROR_STOP) fails the run.
--
-- This test executes the writer insert in its exact shape (literals stand in
-- for the $n parameters): the content-global row converging on the tagged
-- plaintext sha256 with the two-spool copy state (0027) plus the
-- encrypt-on-write crypto envelope and geometry (0028) — plus the
-- sha re-select the writer uses to separate a benign replay from a lost
-- write, the verify/retention timestamp touches, and the negatives
-- (sha/backbone/state/count CHECKs, envelope length + version + format
-- CHECKs, the all-or-nothing crypto completeness guard, epoch range).
--
-- Bytea literals below are synthetic zero fills, never key material.

BEGIN;

DO $$
DECLARE
    row_count_var INTEGER;
    file_id_var TEXT;
BEGIN
    -- Statement 1: full crypto-complete insert. id and created_at are
    -- DB-owned; post_verified_at/last_retention_check stay NULL until the
    -- verify/retention flows stamp them.
    INSERT INTO posted_files (
        plaintext_sha256, backbone_post, backbone_verify,
        article_count, verified_count, original_nzb_sha256,
        post_copy, verify_copy, two_spool_verified, repost_exhausted,
        plaintext_size, article_payload_size, header_nonce,
        file_key_encrypted, file_key_envelope_version, file_key_kek_version,
        file_key_dek_wrapped_nonce, file_key_dek_wrapped,
        file_key_payload_nonce, file_key_payload_format,
        manifest_encrypted, manifest_envelope_version, manifest_kek_version,
        manifest_dek_wrapped_nonce, manifest_dek_wrapped,
        manifest_payload_nonce, manifest_payload_format,
        post_verified_at, last_retention_check
    ) VALUES (
        'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        'spool-post', 'spool-verify',
        11, 0, NULL,
        'present', 'unknown', FALSE, FALSE,
        720896, 721072,
        '\x000000000000000000000000000000000000000000000000'::bytea,
        '\x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f4041'::bytea,
        2, 1,
        '\x000000000000000000000000'::bytea,
        '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
        '\x000000000000000000000000'::bytea,
        1,
        '\x0708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445'::bytea,
        2, 1,
        '\x000000000000000000000000'::bytea,
        '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
        '\x000000000000000000000000'::bytea,
        1,
        NULL, NULL
    )
    ON CONFLICT (plaintext_sha256) DO NOTHING;
    SELECT count(*) INTO row_count_var FROM posted_files
    WHERE plaintext_sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa'
      AND backbone_post = 'spool-post' AND backbone_verify = 'spool-verify'
      AND article_count = 11 AND verified_count = 0
      AND post_copy = 'present' AND verify_copy = 'unknown'
      AND two_spool_verified = FALSE AND repost_exhausted = FALSE
      AND plaintext_size = 720896 AND article_payload_size = 721072
      AND octet_length(header_nonce) = 24
      AND file_key_envelope_version = 2 AND file_key_kek_version = 1
      AND octet_length(file_key_dek_wrapped_nonce) = 12
      AND octet_length(file_key_dek_wrapped) = 48
      AND octet_length(file_key_payload_nonce) = 12
      AND file_key_payload_format = 1
      AND manifest_envelope_version = 2 AND manifest_kek_version = 1
      AND manifest_payload_format = 1
      AND post_verified_at IS NULL AND last_retention_check IS NULL
      AND id IS NOT NULL AND created_at > 0;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'posted_files insert did not land one full crypto-complete row';
    END IF;

    -- The sha re-select (dedupe classification): the replay below must find
    -- this same id.
    SELECT id INTO file_id_var FROM posted_files
    WHERE plaintext_sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';
    IF file_id_var IS NULL THEN
        RAISE EXCEPTION 'posted_files sha re-select found no row';
    END IF;

    -- Statement 2: idempotent replay converges on the sha (zero rows).
    INSERT INTO posted_files (
        plaintext_sha256, backbone_post, backbone_verify,
        article_count, verified_count, original_nzb_sha256,
        post_copy, verify_copy, two_spool_verified, repost_exhausted,
        plaintext_size, article_payload_size, header_nonce,
        file_key_encrypted, file_key_envelope_version, file_key_kek_version,
        file_key_dek_wrapped_nonce, file_key_dek_wrapped,
        file_key_payload_nonce, file_key_payload_format,
        manifest_encrypted, manifest_envelope_version, manifest_kek_version,
        manifest_dek_wrapped_nonce, manifest_dek_wrapped,
        manifest_payload_nonce, manifest_payload_format,
        post_verified_at, last_retention_check
    ) VALUES (
        'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        'spool-post', 'spool-verify',
        11, 0, NULL,
        'present', 'unknown', FALSE, FALSE,
        720896, 721072,
        '\x000000000000000000000000000000000000000000000000'::bytea,
        '\x0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f4041'::bytea,
        2, 1,
        '\x000000000000000000000000'::bytea,
        '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
        '\x000000000000000000000000'::bytea,
        1,
        '\x0708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f202122232425262728292a2b2c2d2e2f303132333435363738393a3b3c3d3e3f404142434445'::bytea,
        2, 1,
        '\x000000000000000000000000'::bytea,
        '\x000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000'::bytea,
        '\x000000000000000000000000'::bytea,
        1,
        NULL, NULL
    )
    ON CONFLICT (plaintext_sha256) DO NOTHING;
    GET DIAGNOSTICS row_count_var = ROW_COUNT;
    IF row_count_var <> 0 THEN
        RAISE EXCEPTION 'posted_files replay inserted instead of converging on the sha';
    END IF;

    -- Statement 3: verify + retention touches.
    UPDATE posted_files
    SET post_verified_at = COALESCE(post_verified_at, 1800000000),
        last_retention_check = 1800000000
    WHERE id = file_id_var;
    SELECT count(*) INTO row_count_var FROM posted_files
    WHERE id = file_id_var
      AND post_verified_at = 1800000000
      AND last_retention_check = 1800000000;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'posted_files verify touch did not stamp both timestamps';
    END IF;

    UPDATE posted_files SET last_retention_check = 1800003600 WHERE id = file_id_var;
    SELECT count(*) INTO row_count_var FROM posted_files
    WHERE id = file_id_var
      AND post_verified_at = 1800000000
      AND last_retention_check = 1800003600;
    IF row_count_var <> 1 THEN
        RAISE EXCEPTION 'posted_files retention touch overwrote post_verified_at';
    END IF;

    -- Negative: non-hex sha.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count)
        VALUES ('NOT-HEX', 'spool-post', 'spool-verify', 1);
        RAISE EXCEPTION 'CHECK accepted a non-hex sha256';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: verify backbone equal to post backbone (single spool
    -- counted twice).
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-post', 1);
        RAISE EXCEPTION 'CHECK accepted an equal backbone pair';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: hostname-shaped backbone label.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'news.Example.COM', 'spool-verify', 1);
        RAISE EXCEPTION 'CHECK accepted a hostname backbone label';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: unknown copy state.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, post_copy)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1, 'gone');
        RAISE EXCEPTION 'CHECK accepted an unknown copy state';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: zero article count.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 0);
        RAISE EXCEPTION 'CHECK accepted a zero article count';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: short header nonce.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, header_nonce)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1,
                '\x000000000000000000000000'::bytea);
        RAISE EXCEPTION 'CHECK accepted a short header nonce';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: short wrapped DEK.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, file_key_dek_wrapped)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1,
                '\x00000000000000000000000000000000'::bytea);
        RAISE EXCEPTION 'CHECK accepted a short wrapped DEK';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: wrong envelope version.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, file_key_envelope_version)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1, 1);
        RAISE EXCEPTION 'CHECK accepted envelope version 1';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: wrong payload format.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, manifest_payload_format)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1, 2);
        RAISE EXCEPTION 'CHECK accepted payload format 2';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: half-filled crypto half (geometry without envelopes).
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, plaintext_size)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1, 720896);
        RAISE EXCEPTION 'completeness guard accepted a half-filled crypto half';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Negative: epoch past the ceiling.
    BEGIN
        INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count, last_retention_check)
        VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
                'spool-post', 'spool-verify', 1, 253402300800);
        RAISE EXCEPTION 'CHECK accepted an epoch past the ceiling';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;
END $$;

ROLLBACK;
