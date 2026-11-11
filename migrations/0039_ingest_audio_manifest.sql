-- First structurally accepted selection survives KV races and job replay.
-- NULL is deliberate for legacy originals; never synthesize a manifest from
-- a flat release/recording array. This adds no catalogue membership or writer.
SET search_path = public, pg_catalog;

ALTER TABLE public.ingest_originals ADD COLUMN IF NOT EXISTS accepted_audio_manifest_v1 TEXT;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_constraint
        WHERE conrelid = 'public.ingest_originals'::regclass
          AND conname = 'ingest_originals_audio_manifest_check'
    ) THEN
        ALTER TABLE public.ingest_originals ADD CONSTRAINT ingest_originals_audio_manifest_check CHECK (
            accepted_audio_manifest_v1 IS NULL OR (
                octet_length(accepted_audio_manifest_v1) <= 524288
                AND jsonb_typeof(accepted_audio_manifest_v1::jsonb) = 'object'
                AND accepted_audio_manifest_v1::jsonb -> 'version' = '1'::jsonb
                AND accepted_audio_manifest_v1::jsonb ->> 'job_id' = job_id
                AND accepted_audio_manifest_v1::jsonb ->> 'original_content_sha256' = content_sha256
                AND jsonb_typeof(accepted_audio_manifest_v1::jsonb -> 'user_id') = 'string'
                AND length(accepted_audio_manifest_v1::jsonb ->> 'user_id') BETWEEN 1 AND 128
            ) IS TRUE
        );
    END IF;
END
$$;

-- The application validates the complete versioned contract before proposing
-- bytes and after reading the winner. SQL backstops root identity and size;
-- it does not turn structurally accepted hints into trusted mirror evidence.
CREATE OR REPLACE FUNCTION public.guard_ingest_audio_manifest()
RETURNS trigger LANGUAGE plpgsql SET search_path = public, pg_catalog AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.accepted_audio_manifest_v1 IS NOT NULL
       AND NEW.accepted_audio_manifest_v1 IS DISTINCT FROM OLD.accepted_audio_manifest_v1 THEN
        RAISE EXCEPTION 'accepted audio manifest is immutable' USING ERRCODE = '23514';
    END IF;
    IF NEW.accepted_audio_manifest_v1 IS NOT NULL AND (
        SELECT j.user_id FROM public.ingest_jobs j WHERE j.id = NEW.job_id
    ) IS DISTINCT FROM (NEW.accepted_audio_manifest_v1::jsonb ->> 'user_id') THEN
        RAISE EXCEPTION 'accepted audio manifest owner mismatch' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END
$$;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_trigger
        WHERE tgrelid = 'public.ingest_originals'::regclass
          AND tgname = 'ingest_originals_audio_manifest_guard'
    ) THEN
        CREATE TRIGGER ingest_originals_audio_manifest_guard
        BEFORE INSERT OR UPDATE OF accepted_audio_manifest_v1 ON public.ingest_originals
        FOR EACH ROW EXECUTE FUNCTION public.guard_ingest_audio_manifest();
    END IF;
END
$$;
