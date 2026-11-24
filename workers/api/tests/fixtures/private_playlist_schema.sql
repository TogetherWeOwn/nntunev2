-- Seed the legacy playlist rehearsal after private_playlist_tables.sql.
INSERT INTO public.libraries (user_id, name)
SELECT DISTINCT owner_id, 'Rehearsal Library' FROM pg_temp.fixture_memberships;
INSERT INTO public.library_tracks (library_id, track_id)
SELECT l.id, m.track_id FROM public.libraries l JOIN pg_temp.fixture_memberships m ON m.owner_id = l.user_id;
WITH inactive AS (
    INSERT INTO public.libraries (user_id, name, deleting_at)
    VALUES ('fixture-user-owner', 'Fenced Library', now()) RETURNING id
)
INSERT INTO public.library_tracks (library_id, track_id)
SELECT inactive.id, t.id FROM inactive, pg_temp.fixture_tracks t
WHERE t.handle IN ('inactive-track', 'trkRpMiddle00000000000');

-- Require an explicit trusted mapping even when the legacy ref resembles a canonical ID.
INSERT INTO public.playlist_tracks (playlist_id, track_id, position, is_legacy)
SELECT p.id, r.track_id, r.position, true
FROM public.playlists p
JOIN pg_temp.legacy_resolution r ON r.playlist_id = p.id
WHERE p.items ->> r.position = r.legacy_ref;

-- Pin the existing dormant row's handle so Rust can assert the stored identity.
UPDATE public.playlist_tracks SET id = '00000000-0000-0000-0000-000000000001'
WHERE playlist_id = 9006 AND position = 1;
