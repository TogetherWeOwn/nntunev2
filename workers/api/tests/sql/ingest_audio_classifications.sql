-- The accepted audio classification checkpoint is stored once per original, first writer wins,
-- and is bound to the job, the owning user and the original content hash.
-- Runs inside the disposable migration lane's transaction. The write path is
-- the exact production statement passed in as :audio_classifications_store_sql; it is
-- never copied here. Concurrent-writer contention is not exercised by this
-- single-session script.
BEGIN;

\ir ../../../../migrations/0040_ingest_audio_classifications.sql

CREATE TEMPORARY TABLE audio_classifications_store_query (sql TEXT NOT NULL);
INSERT INTO audio_classifications_store_query VALUES (:'audio_classifications_store_sql');

-- Runs the production statement; returns NULL when it matched no row.
CREATE FUNCTION pg_temp.store_checkpoint(job TEXT, owner_id TEXT, checkpoint TEXT, original_hash TEXT)
RETURNS TEXT LANGUAGE plpgsql AS $$
DECLARE
    stored TEXT;
BEGIN
    EXECUTE (SELECT sql FROM audio_classifications_store_query)
        INTO stored USING job, owner_id, checkpoint, original_hash;
    RETURN stored;
END
$$;

-- Counts statement rows so "no rows" is distinguished from a stored NULL.
CREATE FUNCTION pg_temp.store_checkpoint_rows(job TEXT, owner_id TEXT, checkpoint TEXT, original_hash TEXT)
RETURNS INTEGER LANGUAGE plpgsql AS $$
DECLARE
    rows_seen INTEGER := 0;
    r RECORD;
BEGIN
    FOR r IN EXECUTE (SELECT sql FROM audio_classifications_store_query)
        USING job, owner_id, checkpoint, original_hash LOOP
        rows_seen := rows_seen + 1;
    END LOOP;
    RETURN rows_seen;
END
$$;

CREATE FUNCTION pg_temp.class_json(job TEXT, owner_id TEXT, original_hash TEXT, label TEXT DEFAULT 'a')
RETURNS TEXT LANGUAGE sql AS $$
    SELECT jsonb_build_object('version', 1, 'job_id', job, 'user_id', owner_id,
                              'original_content_sha256', original_hash, 'label', label)::text
$$;

-- True when the statement raised any error (value is checked separately).
CREATE FUNCTION pg_temp.store_fails(job TEXT, owner_id TEXT, checkpoint TEXT, original_hash TEXT)
RETURNS BOOLEAN LANGUAGE plpgsql AS $$
BEGIN
    PERFORM pg_temp.store_checkpoint(job, owner_id, checkpoint, original_hash);
    RETURN false;
EXCEPTION WHEN OTHERS THEN
    RETURN true;
END
$$;

CREATE FUNCTION pg_temp.stored(job TEXT) RETURNS TEXT LANGUAGE sql AS $$
    SELECT accepted_audio_classifications_v1 FROM public.ingest_originals WHERE job_id = job
$$;

INSERT INTO public.users (id, email, created_at) VALUES
    ('user-class-a', 'class-a@example.com', 1700000000),
    ('user-class-b', 'class-b@example.com', 1700000000),
    ('user-class-gone', 'class-gone@example.com', 1700000000);

INSERT INTO public.ingest_jobs (id, user_id, idempotency_key, accepted_at, status, content_sha256) VALUES
    ('class-job-1', 'user-class-a', 'class-job-1', 1700000000, 'queued', repeat('a', 64)),
    ('class-job-2', 'user-class-a', 'class-job-2', 1700000000, 'queued', repeat('b', 64)),
    ('class-job-legacy', 'user-class-a', 'class-job-legacy', 1700000000, 'queued', repeat('c', 64)),
    ('class-job-skew', 'user-class-a', 'class-job-skew', 1700000000, 'queued', repeat('d', 64)),
    ('class-job-gone', 'user-class-gone', 'class-job-gone', 1700000000, 'queued', repeat('e', 64));

INSERT INTO public.ingest_originals
    (job_id, content_sha256, nzb_bytes, nzb_ref_encrypted, nzb_ref_envelope_version,
     nzb_ref_kek_version, nzb_ref_dek_wrapped_nonce, nzb_ref_dek_wrapped,
     nzb_ref_payload_nonce, nzb_ref_payload_format, created_at)
SELECT v.job_id, v.hash, 10, '\x00'::bytea, 2, 1, decode(repeat('00', 12), 'hex'),
       decode(repeat('00', 48), 'hex'), decode(repeat('00', 12), 'hex'), 1, 1700000000
FROM (VALUES
    ('class-job-1', repeat('a', 64)),
    ('class-job-2', repeat('b', 64)),
    ('class-job-legacy', repeat('c', 64)),
    -- Original hash deliberately disagrees with the job's content hash.
    ('class-job-skew', repeat('f', 64)),
    ('class-job-gone', repeat('e', 64))
) AS v(job_id, hash);

DO $$
DECLARE
    first_checkpoint TEXT := pg_temp.class_json('class-job-1', 'user-class-a', repeat('a', 64), 'first');
    second_checkpoint TEXT := pg_temp.class_json('class-job-1', 'user-class-a', repeat('a', 64), 'second');
    got TEXT;
    base_len INTEGER;
    padded TEXT;
BEGIN
    -- Legacy rows stay NULL and the column is nullable text.
    IF EXISTS (SELECT 1 FROM public.ingest_originals WHERE accepted_audio_classifications_v1 IS NOT NULL) THEN
        RAISE EXCEPTION 'new column was not NULL for existing originals';
    END IF;
    IF NOT EXISTS (
        SELECT 1 FROM information_schema.columns
        WHERE table_schema = 'public' AND table_name = 'ingest_originals'
          AND column_name = 'accepted_audio_classifications_v1'
          AND data_type = 'text' AND is_nullable = 'YES'
    ) THEN
        RAISE EXCEPTION 'accepted_audio_classifications_v1 is not nullable TEXT';
    END IF;

    -- First accept stores and returns the proposal.
    got := pg_temp.store_checkpoint('class-job-1', 'user-class-a', first_checkpoint, repeat('a', 64));
    IF got IS DISTINCT FROM first_checkpoint OR pg_temp.stored('class-job-1') IS DISTINCT FROM first_checkpoint THEN
        RAISE EXCEPTION 'first accept did not store the proposed checkpoint';
    END IF;

    -- A different valid proposal returns the original winner and changes nothing.
    got := pg_temp.store_checkpoint('class-job-1', 'user-class-a', second_checkpoint, repeat('a', 64));
    IF got IS DISTINCT FROM first_checkpoint OR pg_temp.stored('class-job-1') IS DISTINCT FROM first_checkpoint THEN
        RAISE EXCEPTION 'a later proposal replaced or masked the first checkpoint';
    END IF;
    -- The identical proposal is idempotent.
    got := pg_temp.store_checkpoint('class-job-1', 'user-class-a', first_checkpoint, repeat('a', 64));
    IF got IS DISTINCT FROM first_checkpoint THEN
        RAISE EXCEPTION 'identical replay did not return the stored checkpoint';
    END IF;

    -- Wrong user, wrong job, wrong original hash, job/original hash skew: no rows, no write.
    IF pg_temp.store_checkpoint_rows('class-job-2', 'user-class-b',
           pg_temp.class_json('class-job-2', 'user-class-b', repeat('b', 64)), repeat('b', 64)) <> 0
       OR pg_temp.store_checkpoint_rows('class-job-missing', 'user-class-a',
           pg_temp.class_json('class-job-missing', 'user-class-a', repeat('b', 64)), repeat('b', 64)) <> 0
       OR pg_temp.store_checkpoint_rows('class-job-2', 'user-class-a',
           pg_temp.class_json('class-job-2', 'user-class-a', repeat('9', 64)), repeat('9', 64)) <> 0
       OR pg_temp.store_checkpoint_rows('class-job-skew', 'user-class-a',
           pg_temp.class_json('class-job-skew', 'user-class-a', repeat('f', 64)), repeat('f', 64)) <> 0
       OR pg_temp.store_checkpoint_rows('class-job-skew', 'user-class-a',
           pg_temp.class_json('class-job-skew', 'user-class-a', repeat('d', 64)), repeat('d', 64)) <> 0 THEN
        RAISE EXCEPTION 'a mismatched user, job or hash matched a row';
    END IF;
    IF pg_temp.stored('class-job-2') IS NOT NULL OR pg_temp.stored('class-job-skew') IS NOT NULL THEN
        RAISE EXCEPTION 'a mismatched write stored a checkpoint';
    END IF;

    -- A deleting writer is fenced out.
    UPDATE public.users SET deleting_at = to_timestamp(1700000001) WHERE id = 'user-class-gone';
    IF pg_temp.store_checkpoint_rows('class-job-gone', 'user-class-gone',
           pg_temp.class_json('class-job-gone', 'user-class-gone', repeat('e', 64)), repeat('e', 64)) <> 0
       OR pg_temp.stored('class-job-gone') IS NOT NULL THEN
        RAISE EXCEPTION 'a deleting user still stored a checkpoint';
    END IF;

    -- Invalid proposals fail and leave the row untouched.
    base_len := octet_length(pg_temp.class_json('class-job-2', 'user-class-a', repeat('b', 64), ''));
    padded := pg_temp.class_json('class-job-2', 'user-class-a', repeat('b', 64), repeat('x', 524288 - base_len));
    IF octet_length(padded) <> 524288 THEN
        RAISE EXCEPTION 'bound fixture is % bytes', octet_length(padded);
    END IF;
    IF pg_temp.store_fails('class-job-2', 'user-class-a',
           pg_temp.class_json('class-job-2', 'user-class-a', repeat('b', 64), repeat('x', 524289 - base_len)),
           repeat('b', 64))
       IS NOT TRUE THEN
        RAISE EXCEPTION 'a checkpoint over the byte bound was accepted';
    END IF;
    IF pg_temp.stored('class-job-2') IS NOT NULL THEN
        RAISE EXCEPTION 'oversized checkpoint left a value';
    END IF;
    IF pg_temp.store_fails('class-job-2', 'user-class-a', 'not json', repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a', '[]', repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a', '"text"', repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a', 'null', repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a', '{}', repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           jsonb_build_object('version', 2, 'job_id', 'class-job-2', 'user_id', 'user-class-a',
                              'original_content_sha256', repeat('b', 64))::text, repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           pg_temp.class_json('class-job-1', 'user-class-a', repeat('b', 64)), repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           pg_temp.class_json('class-job-2', 'user-class-a', repeat('a', 64)), repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           pg_temp.class_json('class-job-2', '', repeat('b', 64)), repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           jsonb_build_object('version', 1, 'job_id', 'class-job-2',
                              'original_content_sha256', repeat('b', 64))::text, repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           jsonb_build_object('version', 1, 'user_id', 'user-class-a',
                              'original_content_sha256', repeat('b', 64))::text, repeat('b', 64)) IS NOT TRUE
       OR pg_temp.store_fails('class-job-2', 'user-class-a',
           jsonb_build_object('version', 1, 'job_id', 'class-job-2', 'user_id', 'user-class-a')::text,
           repeat('b', 64)) IS NOT TRUE THEN
        RAISE EXCEPTION 'an invalid checkpoint shape, id or digest was accepted';
    END IF;
    IF pg_temp.stored('class-job-2') IS NOT NULL THEN
        RAISE EXCEPTION 'a rejected checkpoint left a value';
    END IF;

    -- Exactly at the byte bound is accepted.
    got := pg_temp.store_checkpoint('class-job-2', 'user-class-a', padded, repeat('b', 64));
    IF got IS DISTINCT FROM padded THEN
        RAISE EXCEPTION 'a checkpoint at the byte bound was refused';
    END IF;
END
$$;

-- Immutability holds against direct writes too: replacement and clearing are
-- refused, while rewriting the same exact text is allowed.
DO $$
DECLARE
    kept TEXT := pg_temp.stored('class-job-1');
BEGIN
    BEGIN
        UPDATE public.ingest_originals
        SET accepted_audio_classifications_v1 = pg_temp.class_json('class-job-1', 'user-class-a', repeat('a', 64), 'other')
        WHERE job_id = 'class-job-1';
        RAISE EXCEPTION 'a stored checkpoint was replaced';
    EXCEPTION WHEN OTHERS THEN
        IF SQLERRM = 'a stored checkpoint was replaced' THEN RAISE; END IF;
    END;
    BEGIN
        UPDATE public.ingest_originals SET accepted_audio_classifications_v1 = NULL WHERE job_id = 'class-job-1';
        RAISE EXCEPTION 'a stored checkpoint was cleared';
    EXCEPTION WHEN OTHERS THEN
        IF SQLERRM = 'a stored checkpoint was cleared' THEN RAISE; END IF;
    END;
    UPDATE public.ingest_originals SET accepted_audio_classifications_v1 = kept WHERE job_id = 'class-job-1';
    IF pg_temp.stored('class-job-1') IS DISTINCT FROM kept THEN
        RAISE EXCEPTION 'immutability checks changed the stored checkpoint';
    END IF;
    -- Unrelated column updates on a populated row are not blocked.
    UPDATE public.ingest_originals SET nzb_bytes = nzb_bytes + 1 WHERE job_id = 'class-job-1';
END
$$;

-- Replaying the migration preserves stored values and legacy NULLs.
CREATE TEMP TABLE audio_classifications_before_replay ON COMMIT DROP AS
SELECT job_id, accepted_audio_classifications_v1 AS checkpoint FROM public.ingest_originals;

\ir ../../../../migrations/0040_ingest_audio_classifications.sql

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM public.ingest_originals o
        JOIN audio_classifications_before_replay b USING (job_id)
        WHERE o.accepted_audio_classifications_v1 IS DISTINCT FROM b.checkpoint
    ) THEN
        RAISE EXCEPTION 'migration replay changed stored checkpoints';
    END IF;
    IF pg_temp.stored('class-job-legacy') IS NOT NULL
       OR pg_temp.stored('class-job-1') IS NULL THEN
        RAISE EXCEPTION 'migration replay lost a stored value or filled a legacy row';
    END IF;
    -- The replayed guard still enforces first-writer-wins.
    BEGIN
        UPDATE public.ingest_originals SET accepted_audio_classifications_v1 = NULL WHERE job_id = 'class-job-1';
        RAISE EXCEPTION 'replayed migration lost immutability';
    EXCEPTION WHEN OTHERS THEN
        IF SQLERRM = 'replayed migration lost immutability' THEN RAISE; END IF;
    END;
END
$$;

ROLLBACK;
