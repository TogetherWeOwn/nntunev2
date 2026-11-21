-- Only prepare_migrated_stores.sh runs this, against its disposable app DB.
BEGIN ISOLATION LEVEL READ COMMITTED;
CREATE TEMP TABLE fixture_cases AS
SELECT (c->'playlist'->>'internal_id')::bigint AS playlist_id,
    c->'playlist' AS playlist, c->'entries' AS entries
FROM jsonb_array_elements(:'corpus'::jsonb->'cases') c;

INSERT INTO public.users (id, email, created_at)
SELECT DISTINCT playlist->>'owner_user_id', (playlist->>'owner_user_id') || '@example.com', 1
FROM fixture_cases;
INSERT INTO public.playlists (id, public_id, owner_user_id, name, items, is_public, created_at)
SELECT playlist_id, playlist->>'public_id', playlist->>'owner_user_id', playlist->>'name',
    playlist->'items', (playlist->>'is_public')::boolean, 1
FROM fixture_cases;
CREATE TEMP TABLE legacy_before AS SELECT to_jsonb(p) AS row FROM public.playlists p;

CREATE TEMP TABLE fixture_tracks AS
SELECT handle, gen_random_uuid() AS id FROM (
    SELECT DISTINCT e->>'track_public_id' AS handle
    FROM fixture_cases c CROSS JOIN LATERAL jsonb_array_elements(c.entries) e
    WHERE e->>'status' = 'resolved'
    UNION ALL SELECT 'new-track'
) handles;
INSERT INTO public.tracks (id, release_id, title, medium_no, track_no)
SELECT id, gen_random_uuid(), handle, 1, 1 FROM fixture_tracks;

-- Fixture-provided explicit resolution is test input, not a production resolver.
CREATE TEMP TABLE legacy_resolution (
    playlist_id bigint, position integer, legacy_ref text, track_id uuid,
    PRIMARY KEY (playlist_id, position)
);
INSERT INTO legacy_resolution
SELECT c.playlist_id, (e->>'position')::integer, e->>'legacy_ref', t.id
FROM fixture_cases c CROSS JOIN LATERAL jsonb_array_elements(c.entries) e
JOIN fixture_tracks t ON t.handle = e->>'track_public_id'
WHERE e->>'status' = 'resolved';
-- A stale mapping for the wrong ref must not fill an unresolved slot.
INSERT INTO legacy_resolution
SELECT 9002, 1, 'not-the-original-ref', id FROM fixture_tracks WHERE handle = 'new-track';
CREATE TEMP TABLE member_tracks (owner_id text, track_id uuid, PRIMARY KEY (owner_id, track_id));
INSERT INTO member_tracks
SELECT 'fixture-user-owner', id FROM fixture_tracks
WHERE handle IN ('new-track', 'trkRpRepeat00000000000');
INSERT INTO member_tracks
SELECT 'fixture-user-other', id FROM fixture_tracks
WHERE handle IN ('new-track', 'trkRpMiddle00000000000');

SAVEPOINT applied_legacy;
SELECT :'playlist_schema' \gexec
SELECT :'playlist_crud' \gexec

DO $$
DECLARE
    owner constant text := 'fixture-user-owner';
    other constant text := 'fixture-user-other';
    repeated constant text := 'plRepeatedRefs00000000';
    fresh constant text := 'zzPrivatePlaylist00000';
    unresolved constant text := 'plAllUnresolved0000000';
    missing constant text := 'noSuchPlaylist00000000';
    new_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'new-track');
    old_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'trkRpRepeat00000000000');
    entry uuid;
    operation uuid;
BEGIN
    IF (SELECT count(*) FROM pg_temp.fixture_cases) < 8 OR EXISTS (
        (SELECT to_jsonb(p) - 'deleting_at' FROM public.playlists p)
        EXCEPT TABLE pg_temp.legacy_before
    ) OR (SELECT count(*) FROM public.playlists) <> (SELECT count(*) FROM pg_temp.legacy_before) THEN
        RAISE EXCEPTION 'rehearsal changed legacy playlist rows';
    END IF;
    IF EXISTS (
        SELECT 1 FROM pg_temp.fixture_cases c
        CROSS JOIN LATERAL jsonb_array_elements(c.entries) e
        LEFT JOIN pg_temp.fixture_tracks t ON t.handle = e->>'track_public_id'
        LEFT JOIN public.playlist_tracks p ON p.playlist_id = c.playlist_id
            AND p.position = (e->>'position')::int
        WHERE (e->>'status' = 'resolved' AND (p.track_id IS DISTINCT FROM t.id OR p.is_legacy IS DISTINCT FROM true))
           OR (e->>'status' = 'unresolved' AND p.id IS NOT NULL)
    ) OR (SELECT count(*) FROM public.playlist_tracks) <> (
        SELECT count(*) FROM pg_temp.fixture_cases c, jsonb_array_elements(c.entries) e
        WHERE e->>'status' = 'resolved'
    ) THEN
        RAISE EXCEPTION 'backfill lost a resolved position or invented a resolution';
    END IF;
    IF EXISTS (SELECT FROM read_playlist(other, repeated))
        OR EXISTS (SELECT FROM read_playlist(other, missing))
        OR EXISTS (SELECT FROM read_playlist(other, 'plMixedResUnres0000000'))
        OR EXISTS (SELECT FROM list_playlists(other, NULL, 50) p WHERE p.id = repeated)
        OR rename_playlist(other, repeated, 'Not allowed') IS NOT NULL
        OR rename_playlist(other, missing, 'Not allowed') IS NOT NULL
        OR add_track(other, repeated, new_track) IS NOT NULL
        OR add_track(other, missing, new_track) IS NOT NULL
        OR delete_playlist(other, repeated) IS NOT NULL
        OR delete_playlist(other, missing) IS NOT NULL THEN
        RAISE EXCEPTION 'non-owner operation differed from missing';
    END IF;
    IF (SELECT r.legacy_items FROM read_playlist(owner, repeated) r) IS DISTINCT FROM
        (SELECT playlist->'items' FROM pg_temp.fixture_cases WHERE playlist_id = 9006)
        OR (SELECT jsonb_agg(e->'position' ORDER BY ord) FROM read_playlist(owner, repeated) r,
                jsonb_array_elements(r.entries) WITH ORDINALITY AS x(e, ord) WHERE e->>'track_id' = old_track::text)
                IS DISTINCT FROM '[0, 2, 3]'::jsonb
        OR add_track(owner, repeated, old_track) IS NOT NULL THEN
        RAISE EXCEPTION 'repeats must persist in order without allowing a duplicate append';
    END IF;
    entry := add_track(owner, unresolved, new_track);
    IF entry IS NULL OR (SELECT r.entries->0->>'position' FROM read_playlist(owner, unresolved) r)
            IS DISTINCT FROM '2' THEN
        RAISE EXCEPTION 'append must not reuse an unresolved legacy position';
    END IF;
    IF create_playlist(owner, fresh, '   ') IS NOT NULL THEN
        RAISE EXCEPTION 'blank name was accepted';
    END IF;
    IF create_playlist(owner, fresh, '  夜の音楽  ') IS DISTINCT FROM fresh THEN
        RAISE EXCEPTION 'create did not return an opaque handle';
    END IF;
    IF (SELECT p.name FROM list_playlists(owner, NULL, 1) p WHERE p.id = fresh)
            IS DISTINCT FROM '夜の音楽'
        OR EXISTS (SELECT FROM list_playlists(owner, fresh, 1) p WHERE p.id = fresh)
        OR NOT EXISTS (SELECT FROM read_playlist(owner, fresh) r WHERE r.entries = '[]'::jsonb
            AND r.legacy_items = '[]'::jsonb) THEN
        RAISE EXCEPTION 'empty playlist did not persist or page correctly';
    END IF;
    IF rename_playlist(owner, fresh, 'Updated') IS DISTINCT FROM fresh THEN
        RAISE EXCEPTION 'rename did not return the same opaque handle';
    END IF;
    IF (SELECT r.name FROM read_playlist(owner, fresh) r) IS DISTINCT FROM 'Updated'
        OR add_track(owner, fresh, (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'trkRpMiddle00000000000')) IS NOT NULL THEN
        RAISE EXCEPTION 'rename did not persist or foreign membership was accepted';
    END IF;
    entry := add_track(owner, fresh, new_track);
    IF entry IS NULL OR add_track(owner, fresh, new_track) IS NOT NULL
        OR (SELECT r.entries FROM read_playlist(owner, fresh) r) IS DISTINCT FROM
            jsonb_build_array(jsonb_build_object('id', entry, 'track_id', new_track, 'position', 0)) THEN
        RAISE EXCEPTION 'append did not persist exactly one canonical entry';
    END IF;
    operation := delete_playlist(owner, fresh);
    IF operation IS NULL OR delete_playlist(owner, fresh) IS DISTINCT FROM operation
        OR EXISTS (SELECT FROM read_playlist(owner, fresh))
        OR EXISTS (SELECT FROM list_playlists(owner, NULL, 50) p WHERE p.id = fresh)
        OR rename_playlist(owner, fresh, 'Too late') IS NOT NULL
        OR add_track(owner, fresh, old_track) IS NOT NULL
        OR NOT EXISTS (SELECT FROM public.delete_operations d WHERE d.id = operation
            AND d.user_id = owner AND d.container_id = fresh AND d.scope = 'playlist') THEN
        RAISE EXCEPTION 'delete request did not persist and fence the playlist';
    END IF;
    UPDATE public.users SET deleting_at = now() WHERE id = owner;
    IF EXISTS (SELECT FROM list_playlists(owner, NULL, 50))
        OR create_playlist(owner, 'newPrivatePlaylist0001', 'Too late') IS NOT NULL
        OR rename_playlist(owner, repeated, 'Too late') IS NOT NULL
        OR add_track(owner, repeated, new_track) IS NOT NULL
        OR delete_playlist(owner, repeated) IS NOT NULL THEN
        RAISE EXCEPTION 'account deletion did not fence playlist access';
    END IF;
END;
$$;
SET CONSTRAINTS ALL IMMEDIATE;
ROLLBACK TO applied_legacy;

DO $$
BEGIN
    IF to_regclass('public.playlist_tracks') IS NOT NULL
        OR to_regnamespace('private_playlist_rehearsal') IS NOT NULL
        OR EXISTS (SELECT FROM pg_attribute WHERE attrelid = 'public.playlists'::regclass
            AND attname = 'deleting_at' AND NOT attisdropped)
        OR EXISTS (SELECT to_jsonb(p) FROM public.playlists p EXCEPT TABLE legacy_before)
        OR (SELECT count(*) FROM public.playlists) <> (SELECT count(*) FROM legacy_before)
        OR EXISTS (SELECT FROM public.delete_operations WHERE scope = 'playlist') THEN
        RAISE EXCEPTION 'rollback did not restore the applied legacy shape and data';
    END IF;
END;
$$;
ROLLBACK;
\echo Private playlist rehearsal: upgrade, owner CRUD, duplicate policy and rollback passed
