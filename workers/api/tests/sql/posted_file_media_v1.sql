-- Synthetic zero-filled envelopes/header only; no authentic media or key data.
-- Validate schema shapes, legacy compatibility, immutability and replay, not
-- cryptographic header authentication, usable-state promotion or live rollout.
BEGIN;

DO $$
DECLARE
    base_row posted_files%ROWTYPE;
    probe posted_files%ROWTYPE;
    patch JSONB;
BEGIN
    INSERT INTO posted_files (
        plaintext_sha256, backbone_post, backbone_verify, article_count,
        plaintext_size, article_payload_size, media_format, media_key_mode, media_header,
        file_key_encrypted, file_key_envelope_version, file_key_kek_version,
        file_key_dek_wrapped_nonce, file_key_dek_wrapped, file_key_payload_nonce, file_key_payload_format,
        manifest_encrypted, manifest_envelope_version, manifest_kek_version,
        manifest_dek_wrapped_nonce, manifest_dek_wrapped, manifest_payload_nonce, manifest_payload_format
    ) VALUES (
        repeat('c', 64), 'pool-a', 'pool-b', 1,
        1024, 1040, 'velmora.media.v1', 'random', decode(repeat('00', 48), 'hex'),
        decode(repeat('00', 48), 'hex'), 2, 1,
        decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'), decode(repeat('00', 12), 'hex'), 1,
        decode(repeat('00', 32), 'hex'), 2, 1,
        decode(repeat('00', 12), 'hex'), decode(repeat('00', 48), 'hex'), decode(repeat('00', 12), 'hex'), 1
    ) RETURNING * INTO base_row;
    IF base_row.header_nonce IS NOT NULL OR octet_length(base_row.media_header) <> 48 THEN
        RAISE EXCEPTION 'media metadata did not persist exactly';
    END IF;

    -- Every malformed tuple must fail at insert, not just at the immutable
    -- update trigger. Each subtransaction rolls back its failed insert.
    FOR patch IN SELECT value FROM jsonb_array_elements(jsonb_build_array(
        jsonb_build_object('media_format', 'unknown'),
        jsonb_build_object('media_format', NULL),
        jsonb_build_object('media_key_mode', NULL),
        jsonb_build_object('media_key_mode', 'convergent'),
        jsonb_build_object('media_header', NULL),
        jsonb_build_object('media_header', decode(repeat('00', 24), 'hex')),
        jsonb_build_object('media_header', decode(repeat('00', 47), 'hex')),
        jsonb_build_object('media_header', decode(repeat('00', 49), 'hex')),
        jsonb_build_object('header_nonce', decode(repeat('00', 24), 'hex')),
        jsonb_build_object('media_format', 'legacy'),
        jsonb_build_object('file_key_encrypted', NULL),
        jsonb_build_object('file_key_encrypted', decode(repeat('00', 47), 'hex')),
        jsonb_build_object('file_key_encrypted', decode(repeat('00', 49), 'hex')),
        jsonb_build_object('manifest_encrypted', NULL),
        jsonb_build_object('manifest_payload_format', NULL),
        jsonb_build_object('file_key_dek_wrapped_nonce', NULL)
    )) LOOP
        probe := jsonb_populate_record(NULL::posted_files,
            to_jsonb(base_row) || patch || jsonb_build_object(
                'id', gen_random_uuid()::text, 'plaintext_sha256', repeat('d', 64)));
        BEGIN
            INSERT INTO posted_files SELECT (probe).*;
            RAISE EXCEPTION 'invalid media tuple accepted';
        EXCEPTION WHEN check_violation OR not_null_violation THEN NULL;
        END;
    END LOOP;

    -- Historical complete-envelope and ledger-only shapes still insert with
    -- the historical default discriminator. No migration converts their bytes.
    probe := base_row;
    probe.id := gen_random_uuid()::text;
    probe.plaintext_sha256 := repeat('e', 64);
    probe.media_format := 'legacy';
    probe.media_key_mode := NULL;
    probe.media_header := NULL;
    probe.header_nonce := decode(repeat('00', 24), 'hex');
    INSERT INTO posted_files SELECT (probe).*;
    INSERT INTO posted_files (plaintext_sha256, backbone_post, backbone_verify, article_count)
        VALUES (repeat('f', 64), 'pool-a', 'pool-b', 1);

    BEGIN
        UPDATE posted_files SET media_header = decode(repeat('01', 48), 'hex') WHERE id = base_row.id;
        RAISE EXCEPTION 'media header mutation accepted';
    EXCEPTION WHEN raise_exception THEN
        IF SQLERRM <> 'posted generation custody is immutable' THEN RAISE; END IF;
    END;
    BEGIN
        UPDATE posted_files SET media_key_mode = 'convergent' WHERE id = base_row.id;
        RAISE EXCEPTION 'media mode mutation accepted';
    EXCEPTION WHEN raise_exception THEN
        IF SQLERRM <> 'posted generation custody is immutable' THEN RAISE; END IF;
    END;
    BEGIN
        UPDATE posted_files SET media_format = 'velmora.media.v1' WHERE plaintext_sha256 = repeat('e', 64);
        RAISE EXCEPTION 'legacy relabelling accepted';
    EXCEPTION WHEN raise_exception THEN
        IF SQLERRM <> 'posted generation custody is immutable' THEN RAISE; END IF;
    END;
END $$;

-- Flush deferred constraint triggers before replaying DDL in this transaction.
-- Production migration application and its ordinary replay happen before writes.
SET CONSTRAINTS ALL IMMEDIATE;
CREATE TEMP TABLE media_custody_before AS SELECT id, to_jsonb(p) AS data FROM posted_files p;
\ir ../../../../migrations/0050_posted_file_media_v1.sql
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM media_custody_before b FULL JOIN posted_files p USING (id)
        WHERE b.data IS DISTINCT FROM to_jsonb(p)
    ) THEN
        RAISE EXCEPTION 'media migration replay changed existing custody';
    END IF;
END $$;
ROLLBACK;
