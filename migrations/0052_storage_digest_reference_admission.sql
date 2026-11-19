SET search_path = public, pg_catalog;

-- Only guarded publication may populate this table. No publication writer here.
CREATE TABLE IF NOT EXISTS public.storage_validated_digest_winners (
    object_id UUID PRIMARY KEY REFERENCES public.storage_digest_map_intents (object_id),
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    plaintext_bytes BIGINT NOT NULL,
    plaintext_sha256 BYTEA NOT NULL,
    root_version_id UUID NOT NULL,
    quarantined BOOLEAN NOT NULL DEFAULT FALSE,
    retain_until TIMESTAMPTZ NOT NULL,
    read_until TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    upload_until TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    delete_until TIMESTAMPTZ NOT NULL DEFAULT '-infinity',
    deleting_at TIMESTAMPTZ,
    FOREIGN KEY (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256)
        REFERENCES public.storage_plaintext_digest_claim_intents
            (object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256),
    FOREIGN KEY (object_id, root_version_id)
        REFERENCES public.storage_object_envelope_intents (object_id, root_version_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS storage_validated_digest_winner_scope
    ON public.storage_validated_digest_winners (environment_id, library_id, plaintext_sha256, plaintext_bytes)
    WHERE deleting_at IS NULL;

CREATE TABLE IF NOT EXISTS public.storage_validated_digest_references (
    environment_id UUID NOT NULL,
    library_id UUID NOT NULL,
    reference_id UUID NOT NULL,
    object_id UUID NOT NULL REFERENCES public.storage_validated_digest_winners (object_id),
    PRIMARY KEY (environment_id, library_id, reference_id),
    FOREIGN KEY (object_id, environment_id, library_id)
        REFERENCES public.storage_object_intents (object_id, environment_id, library_id)
);
CREATE INDEX IF NOT EXISTS storage_validated_digest_reference_object
    ON public.storage_validated_digest_references (object_id);
REVOKE ALL ON public.storage_validated_digest_winners, public.storage_validated_digest_references FROM PUBLIC;

DROP TRIGGER IF EXISTS storage_validated_digest_winner_immutable ON public.storage_validated_digest_winners;
CREATE TRIGGER storage_validated_digest_winner_immutable
    BEFORE UPDATE OF object_id, environment_id, library_id, plaintext_bytes, plaintext_sha256, root_version_id
    OR DELETE OR TRUNCATE ON public.storage_validated_digest_winners
    FOR EACH STATEMENT EXECUTE FUNCTION public.storage_object_intent_append_only();
CREATE OR REPLACE FUNCTION public.storage_digest_winner_retirement_guard() RETURNS TRIGGER
LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
BEGIN
    IF (OLD.deleting_at IS NOT NULL AND NEW.deleting_at IS DISTINCT FROM OLD.deleting_at)
       OR (OLD.quarantined AND NOT NEW.quarantined) THEN
        RAISE EXCEPTION 'storage winner retirement cannot be reversed' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END $$;
DROP TRIGGER IF EXISTS storage_digest_winner_retirement ON public.storage_validated_digest_winners;
CREATE TRIGGER storage_digest_winner_retirement BEFORE UPDATE OF deleting_at, quarantined
    ON public.storage_validated_digest_winners FOR EACH ROW
    EXECUTE FUNCTION public.storage_digest_winner_retirement_guard();

-- Every future reference, lease, and retention writer must lock this winner
-- before acting, and refuse new live work on a tombstone. READ COMMITTED gives
-- the reference probe a fresh snapshot *after* the lock wait; a fixed snapshot
-- could miss a reference committed by the previous lock holder.
CREATE OR REPLACE FUNCTION public.lock_storage_digest_winner(
    p_environment UUID, p_library UUID, p_digest BYTEA, p_bytes BIGINT,
    p_object UUID, p_generation UUID, p_root_version UUID
) RETURNS public.storage_validated_digest_winners
LANGUAGE sql VOLATILE SET search_path = pg_catalog, public AS $$
    SELECT w.* FROM public.storage_validated_digest_winners w
    JOIN public.storage_object_intents i USING (object_id)
    WHERE current_setting('transaction_isolation') = 'read committed'
      AND w.object_id = p_object AND i.generation_id = p_generation AND w.root_version_id = p_root_version
      AND w.environment_id = p_environment AND w.library_id = p_library
      AND w.plaintext_sha256 = p_digest AND w.plaintext_bytes = p_bytes
      AND NOT w.quarantined AND w.deleting_at IS NULL
    FOR UPDATE OF w;
$$;

-- Invoker-only. The caller must already authorize this exact reference to
-- this exact object in this scope. A digest or supplied deadline is not authority.
CREATE OR REPLACE FUNCTION public.attach_storage_digest_reference(
    p_environment UUID, p_library UUID, p_digest BYTEA, p_bytes BIGINT,
    p_object UUID, p_generation UUID, p_root_version UUID, p_reference UUID, p_authorized_until TIMESTAMPTZ
) RETURNS BOOLEAN LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE winner public.storage_validated_digest_winners;
BEGIN
    winner := public.lock_storage_digest_winner(p_environment, p_library, p_digest, p_bytes, p_object, p_generation, p_root_version);
    IF winner.object_id IS NULL OR p_reference IS NULL OR p_authorized_until IS NULL
       OR p_authorized_until <= clock_timestamp() OR winner.delete_until > clock_timestamp() THEN
        RETURN FALSE;
    END IF;
    INSERT INTO public.storage_validated_digest_references VALUES (p_environment, p_library, p_reference, p_object)
    ON CONFLICT (environment_id, library_id, reference_id) DO NOTHING;
    IF FOUND AND p_authorized_until <= clock_timestamp() THEN
        -- Another object can contend for the same reference's unique key.
        RAISE EXCEPTION 'reference authorization expired' USING ERRCODE = 'PDE01';
    END IF;
    RETURN FOUND;
EXCEPTION WHEN SQLSTATE 'PDE01' THEN RETURN FALSE;
END $$;

-- Admission only. No locator is returned or deleted; the tombstone is durable.
CREATE OR REPLACE FUNCTION public.admit_storage_digest_delete(
    p_environment UUID, p_library UUID, p_digest BYTEA, p_bytes BIGINT,
    p_object UUID, p_generation UUID, p_root_version UUID
) RETURNS BOOLEAN LANGUAGE plpgsql SET search_path = pg_catalog, public AS $$
DECLARE winner public.storage_validated_digest_winners; checked_at TIMESTAMPTZ;
BEGIN
    winner := public.lock_storage_digest_winner(p_environment, p_library, p_digest, p_bytes, p_object, p_generation, p_root_version);
    checked_at := clock_timestamp();
    IF winner.object_id IS NULL
       OR greatest(winner.retain_until, winner.read_until, winner.upload_until, winner.delete_until) > checked_at
       OR EXISTS (SELECT FROM public.storage_validated_digest_references WHERE object_id = p_object) THEN
        RETURN FALSE;
    END IF;
    UPDATE public.storage_validated_digest_winners SET deleting_at = checked_at WHERE object_id = p_object;
    RETURN FOUND;
END $$;
REVOKE ALL ON FUNCTION public.storage_digest_winner_retirement_guard(),
    public.lock_storage_digest_winner(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID),
    public.attach_storage_digest_reference(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID, UUID, TIMESTAMPTZ),
    public.admit_storage_digest_delete(UUID, UUID, BYTEA, BIGINT, UUID, UUID, UUID) FROM PUBLIC;
