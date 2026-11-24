-- Change the pinned publication after lease acquisition and before issuance
-- re-reads it. Both writes must roll back when the issuer refuses.
CREATE FUNCTION public.test_private_playback_advance_publication() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE public.catalogue_s3_sources SET accepted_seq = accepted_seq + 1
    WHERE track_id = NEW.track_id::uuid;
    RETURN NEW;
END $$;

-- Each refusal is set up independently, leaving the exact bearer unchanged.
CREATE FUNCTION pg_temp.private_playback_state(state text, track text, base_seq bigint) RETURNS void
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE public.invite_codes SET revoked_at = CASE WHEN state = 'invite' THEN 2 END
        WHERE code_hash = 'synthetic-playback-invite';
    UPDATE public.sessions SET revoked = (state = 'session')
        WHERE user_id = 'catalogue-other';
    UPDATE public.sessions SET rotation_counter = (state = 'rotation')::integer
        WHERE user_id = 'catalogue-other';
    UPDATE public.catalogue_s3_sources
    SET accepted_seq = base_seq + (state = 'revision')::integer
    WHERE track_id = track::uuid;
    IF state = 'race' THEN
        CREATE TRIGGER test_private_playback_revision_race
            AFTER INSERT OR UPDATE ON public.playback_leases FOR EACH ROW
            WHEN (NEW.user_id = 'catalogue-other')
            EXECUTE FUNCTION public.test_private_playback_advance_publication();
    END IF;
    IF state = 'lease' THEN
        DROP TRIGGER test_private_playback_revision_race ON public.playback_leases;
        UPDATE public.playback_leases SET epoch = epoch + 1 WHERE user_id = 'catalogue-other';
    END IF;
END $$;
