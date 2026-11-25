WITH job AS MATERIALIZED (
    SELECT j.accepted_seq FROM public.ingest_jobs j
    JOIN (SELECT id FROM public.users WHERE id = $7 AND deleting_at IS NULL FOR SHARE) owner
      ON owner.id = j.user_id
    WHERE j.id = $6 AND j.status = 'done'
      AND current_setting('transaction_isolation') = 'read committed'
    FOR SHARE OF j
), inserted AS (
    INSERT INTO public.tracks (release_id, medium_no, track_no, mbid, title, duration_ms)
    SELECT $1::text::uuid, $2, $3, $4::text::uuid, $5, $8 FROM job
    ON CONFLICT (release_id, medium_no, track_no) DO NOTHING
)
SELECT accepted_seq FROM job
