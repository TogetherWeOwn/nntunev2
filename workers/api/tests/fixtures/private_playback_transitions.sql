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
    IF state = 'lease' THEN
        UPDATE public.playback_leases SET epoch = epoch + 1 WHERE user_id = 'catalogue-other';
    END IF;
END $$;
