-- Rehearsal schema for the service-owned one-source-per-occurrence slot.
-- The object keys immutable locator, winner, map and receipt records.
CREATE TABLE public.catalogue_s3_sources (
    track_id UUID PRIMARY KEY REFERENCES public.tracks (id),
    source_job_id TEXT REFERENCES public.ingest_jobs (id) ON DELETE SET NULL,
    accepted_seq BIGINT NOT NULL,
    object_id UUID NOT NULL REFERENCES public.storage_s3_locator_intents (object_id)
        REFERENCES public.storage_validated_digest_winners (object_id)
);
