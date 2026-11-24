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
    SELECT e->>'track_public_id' AS handle FROM fixture_cases c, jsonb_array_elements(c.entries) e
    WHERE e->>'status' = 'resolved' UNION SELECT 'new-track' UNION SELECT 'inactive-track'
) handles;
INSERT INTO public.tracks (id, release_id, title, medium_no, track_no)
SELECT id, gen_random_uuid(), handle, 1, 1 FROM fixture_tracks;
-- Fixture-provided explicit resolution is test input, not a production resolver.
CREATE TEMP TABLE legacy_resolution AS
SELECT c.playlist_id, (e->>'position')::integer AS position, e->>'legacy_ref' AS legacy_ref, t.id AS track_id
FROM fixture_cases c CROSS JOIN LATERAL jsonb_array_elements(c.entries) e
JOIN fixture_tracks t ON t.handle = e->>'track_public_id' WHERE e->>'status' = 'resolved'
-- A stale mapping for the wrong ref must not fill an unresolved slot.
UNION ALL SELECT 9002, 1, 'not-the-original-ref', id FROM fixture_tracks WHERE handle = 'new-track';
CREATE TEMP TABLE fixture_memberships AS
SELECT 'fixture-user-owner'::text AS owner_id, id AS track_id FROM fixture_tracks
WHERE handle IN ('new-track', 'trkRpRepeat00000000000', 'trkFrAlpha000000000000')
UNION ALL SELECT 'fixture-user-other', id FROM fixture_tracks WHERE handle IN ('new-track', 'trkRpMiddle00000000000');
CREATE TEMP TABLE sequence_before AS SELECT last_value, is_called FROM public.playlists_id_seq;
SAVEPOINT applied_legacy;
-- RESTART makes allocation transactional; plain nextval survives rollback.
ALTER SEQUENCE public.playlists_id_seq RESTART;
SELECT :'playlist_schema' \gexec
SELECT :'playlist_crud' \gexec
CREATE TEMP VIEW crud_state AS
SELECT 'playlist' AS kind, to_jsonb(p) AS data FROM public.playlists p
UNION ALL SELECT 'entry', to_jsonb(e) FROM public.playlist_tracks e
UNION ALL SELECT 'operation', to_jsonb(d) FROM public.delete_operations d;
DO $$
DECLARE
    owner constant text := 'fixture-user-owner';
    other constant text := 'fixture-user-other';
    repeated constant text := 'plRepeatedRefs00000000';
    fresh constant text := 'zzPrivatePlaylist00000';
    unresolved constant text := 'plAllUnresolved0000000';
    missing constant text := 'noSuchPlaylist00000000';
    shared constant text := 'plMixedResUnres0000000';
    denied record;
    new_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'new-track');
    old_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'trkRpRepeat00000000000');
    middle_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'trkRpMiddle00000000000');
    cap_track uuid := (SELECT id FROM pg_temp.fixture_tracks WHERE handle = 'trkFrAlpha000000000000');
    blank constant text := U&'\0009\00A0\3000';
    entry uuid;
    second_entry uuid;
    operation uuid;
    renamed text;
    snapshot jsonb;
BEGIN
    IF (SELECT count(*) FROM pg_temp.fixture_cases) < 8
        OR EXISTS (SELECT to_jsonb(p) - 'deleting_at' FROM public.playlists p EXCEPT TABLE pg_temp.legacy_before)
        OR (SELECT count(*) FROM public.playlists) <> (SELECT count(*) FROM pg_temp.legacy_before) THEN
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
    SELECT jsonb_agg(s ORDER BY s.kind, s.data) INTO snapshot FROM pg_temp.crud_state s;
    FOR denied IN SELECT * FROM (VALUES (other, repeated), (other, missing), (other, shared),
        (owner, shared)) AS cases(actor, handle) LOOP
        IF EXISTS (SELECT FROM read_playlist(denied.actor, denied.handle))
            OR EXISTS (SELECT FROM list_playlists(denied.actor, NULL, 50) WHERE id = denied.handle)
            OR rename_playlist(denied.actor, denied.handle, 'Not allowed') IS NOT NULL
            OR add_track(denied.actor, denied.handle, new_track) IS NOT NULL
            OR delete_playlist(denied.actor, denied.handle) IS NOT NULL THEN
            RAISE EXCEPTION 'private CRUD exposed a foreign, public or missing playlist';
        END IF;
    END LOOP;
    IF (SELECT jsonb_agg(s ORDER BY s.kind, s.data) FROM pg_temp.crud_state s) IS DISTINCT FROM snapshot THEN
        RAISE EXCEPTION 'denied operation changed a playlist, entry or deletion operation';
    END IF;
    IF (SELECT r.entries->1 FROM read_playlist(owner, repeated) r) IS DISTINCT FROM (
        SELECT jsonb_build_object('id', e.id, 'position', 1, 'dormant', true)
        FROM public.playlist_tracks e WHERE e.playlist_id = 9006 AND e.position = 1 AND e.track_id = middle_track
    ) THEN
        RAISE EXCEPTION 'nonmember entry must be a tombstone with its stored track FK preserved';
    END IF;
    IF (SELECT jsonb_agg(e->'position' ORDER BY ord) FROM read_playlist(owner, repeated) r,
        jsonb_array_elements(r.entries) WITH ORDINALITY AS x(e, ord) WHERE e->>'track_id' = old_track::text)
        IS DISTINCT FROM '[0, 2, 3]'::jsonb
        OR add_track(owner, repeated, old_track) IS NOT NULL THEN
        RAISE EXCEPTION 'repeats must persist in order without allowing a duplicate append';
    END IF;
    entry := add_track(owner, unresolved, new_track);
    IF entry IS NULL OR NOT EXISTS (SELECT FROM read_playlist(owner, unresolved) r
        JOIN pg_temp.fixture_cases c ON c.playlist->>'public_id' = r.id
        WHERE r.entries->0->>'position' = '2' AND r.legacy_items = c.playlist->'items') THEN
        RAISE EXCEPTION 'owner read must keep unresolved refs and append after the legacy positions';
    END IF;
    INSERT INTO public.playlist_tracks (playlist_id, track_id, position)
    SELECT 9003, new_track, position FROM generate_series(3, 498) AS series(position);
    entry := add_track(owner, unresolved, old_track);
    second_entry := add_track(owner, unresolved, cap_track);
    IF (SELECT position FROM public.playlist_tracks WHERE id = entry) IS DISTINCT FROM 499
        OR second_entry IS NOT NULL OR (SELECT count(*) FROM public.playlist_tracks WHERE playlist_id = 9003) <> 498 THEN
        RAISE EXCEPTION 'append must fill position 499 and reject position 500';
    END IF;
    IF create_playlist(owner, fresh, blank) IS NOT NULL
        OR create_playlist(owner, fresh, blank || '夜の音楽' || blank) IS DISTINCT FROM fresh THEN
        RAISE EXCEPTION 'create must reject blank names and return an opaque handle for a valid name';
    END IF;
    IF (SELECT p.name FROM list_playlists(owner, NULL, 1) p WHERE p.id = fresh)
            IS DISTINCT FROM '夜の音楽'
        OR (SELECT p.id FROM list_playlists(owner, fresh, 1) p) IS DISTINCT FROM repeated
        OR NOT EXISTS (SELECT FROM read_playlist(owner, fresh) r WHERE r.entries = '[]'::jsonb
            AND r.legacy_items = '[]'::jsonb) THEN
        RAISE EXCEPTION 'empty playlist did not persist or page correctly';
    END IF;
    renamed := rename_playlist(owner, fresh, blank || 'Updated' || blank);
    IF renamed IS DISTINCT FROM fresh OR rename_playlist(owner, fresh, blank) IS NOT NULL
        OR (SELECT r.name FROM read_playlist(owner, fresh) r) IS DISTINCT FROM 'Updated'
        OR add_track(owner, fresh, middle_track) IS NOT NULL THEN
        RAISE EXCEPTION 'rename did not persist or foreign membership was accepted';
    END IF;
    entry := add_track(owner, fresh, new_track);
    IF entry IS NULL OR add_track(owner, fresh, new_track) IS NOT NULL
        OR (SELECT r.entries FROM read_playlist(owner, fresh) r) IS DISTINCT FROM
            jsonb_build_array(jsonb_build_object('id', entry, 'track_id', new_track, 'position', 0)) THEN
        RAISE EXCEPTION 'append did not persist exactly one canonical entry';
    END IF;
    second_entry := add_track(owner, fresh, old_track);
    UPDATE public.playlist_tracks SET track_id = new_track WHERE id = second_entry;
    IF second_entry IS NULL OR (SELECT r.entries FROM read_playlist(owner, fresh) r) IS DISTINCT FROM
        jsonb_build_array(jsonb_build_object('id', entry, 'track_id', new_track, 'position', 0),
            jsonb_build_object('id', second_entry, 'track_id', new_track, 'position', 1)) THEN
        RAISE EXCEPTION 'catalogue remap must retain both new entry IDs and positions';
    END IF;
    operation := delete_playlist(owner, fresh);
    IF operation IS NULL OR delete_playlist(owner, fresh) IS DISTINCT FROM operation
        OR EXISTS (SELECT FROM read_playlist(owner, fresh))
        OR EXISTS (SELECT FROM list_playlists(owner, NULL, 50) p WHERE p.id = fresh)
        OR rename_playlist(owner, fresh, 'Too late') IS NOT NULL
        OR add_track(owner, fresh, old_track) IS NOT NULL
        OR NOT EXISTS (SELECT FROM public.delete_operations d WHERE d.id = operation
            AND d.user_id = owner AND d.container_id = fresh AND d.scope = 'playlist' AND d.state = 'pending') THEN
        RAISE EXCEPTION 'delete request did not persist and fence the playlist';
    END IF;
    INSERT INTO public.delete_operations (user_id, scope, container_id, started_at, state)
    VALUES (owner, 'playlist', 'removedPlaylist0000000', now(), 'done') RETURNING id INTO operation;
    IF delete_playlist(owner, 'removedPlaylist0000000') IS DISTINCT FROM operation
        OR delete_playlist(other, 'removedPlaylist0000000') IS NOT NULL
        OR delete_playlist(owner, missing) IS NOT NULL THEN
        RAISE EXCEPTION 'retry of a completed removal must remain owner-scoped';
    END IF;
    IF EXISTS (SELECT FROM pg_temp.legacy_before b LEFT JOIN public.playlists p ON p.id = (b.row->>'id')::bigint
        WHERE p.items IS DISTINCT FROM b.row->'items') THEN
        RAISE EXCEPTION 'normalized CRUD rewrote the legacy items array';
    END IF;
    UPDATE public.users SET deleting_at = now() WHERE id = owner;
    renamed := rename_playlist(owner, repeated, 'Too late');
    IF renamed IS NOT NULL OR NOT EXISTS (SELECT FROM public.playlists p JOIN pg_temp.fixture_cases c
        ON p.id = c.playlist_id WHERE p.public_id = repeated AND p.name = c.playlist->>'name')
        OR EXISTS (SELECT FROM list_playlists(owner, NULL, 50))
        OR create_playlist(owner, 'newPrivatePlaylist0001', 'Too late') IS NOT NULL
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
        OR to_regclass('public.libraries') IS NOT NULL OR to_regclass('public.library_tracks') IS NOT NULL
        OR to_regnamespace('private_playlist_rehearsal') IS NOT NULL
        OR EXISTS (SELECT FROM pg_attribute WHERE attrelid = 'public.playlists'::regclass
            AND attname = 'deleting_at' AND NOT attisdropped)
        OR EXISTS (SELECT to_jsonb(p) FROM public.playlists p EXCEPT TABLE legacy_before)
        OR (SELECT count(*) FROM public.playlists) <> (SELECT count(*) FROM legacy_before)
        OR EXISTS (SELECT FROM public.delete_operations WHERE scope = 'playlist')
        OR EXISTS (SELECT last_value, is_called FROM public.playlists_id_seq EXCEPT TABLE sequence_before) THEN
        RAISE EXCEPTION 'rollback did not restore the applied legacy shape, data and sequence';
    END IF;
END;
$$;
-- Reapply and commit only in the throwaway clone, so a second session can see the fixture.
SELECT :'playlist_schema' \gexec
SELECT :'playlist_crud' \gexec
CREATE TEMP TABLE race_cases AS
SELECT m.owner_id, create_playlist(m.owner_id, rpad(m.owner_id, 22, '0'), 'Fence') AS handle,
    (SELECT id FROM fixture_tracks WHERE handle = 'new-track') AS admitted, m.track_id AS late,
    t.handle = 'trkRpMiddle00000000000' AS account_scope
FROM member_tracks m JOIN fixture_tracks t ON t.id = m.track_id
WHERE t.handle IN ('trkRpRepeat00000000000', 'trkRpMiddle00000000000');
COMMIT;
CREATE EXTENSION dblink WITH SCHEMA public;
SET search_path = private_playlist_rehearsal, pg_catalog;
SET statement_timeout = '20s';
SELECT public.dblink_connect('writer', 'dbname=nntune_playlist_ci connect_timeout=5');
SELECT public.dblink_exec('writer', 'SET search_path = private_playlist_rehearsal, pg_catalog; SET statement_timeout = 5000');
DO $$
DECLARE
    c record;
    writer_pid integer := (SELECT pid FROM public.dblink('writer', 'SELECT pg_backend_pid()') AS w(pid integer));
BEGIN
    IF (SELECT count(DISTINCT account_scope) FROM pg_temp.race_cases) <> 2 THEN
        RAISE EXCEPTION 'both deletion scopes must be tested';
    END IF;
    FOR c IN SELECT * FROM pg_temp.race_cases LOOP
        PERFORM public.dblink_exec('writer', 'BEGIN');
        IF (SELECT id FROM public.dblink('writer', format('SELECT add_track(%L,%L,%L)',
            c.owner_id, c.handle, c.admitted)) AS r(id uuid)) IS NULL THEN
            RAISE EXCEPTION 'append was not admitted before the fence';
        END IF;
        BEGIN
            -- Non-key updates conflict with the explicit locks, not an FK key-share lock.
            IF c.account_scope THEN
                PERFORM FROM public.users WHERE id = c.owner_id FOR NO KEY UPDATE NOWAIT;
            ELSE
                PERFORM FROM public.playlists WHERE public_id = c.handle FOR NO KEY UPDATE NOWAIT;
            END IF;
            RAISE EXCEPTION 'admitted append did not hold the fence row lock (account=%)', c.account_scope;
        EXCEPTION WHEN lock_not_available THEN NULL;
        END;
        PERFORM public.dblink_exec('writer', 'COMMIT');
        IF c.account_scope THEN
            UPDATE public.users SET deleting_at = now() WHERE id = c.owner_id;
        ELSIF delete_playlist(c.owner_id, c.handle) IS NULL THEN
            RAISE EXCEPTION 'playlist fence could not be committed';
        END IF;
        IF public.dblink_send_query('writer', format('SELECT add_track(%L,%L,%L)', c.owner_id, c.handle, c.late)) <> 1 THEN
            RAISE EXCEPTION 'could not send late append';
        END IF;
        PERFORM await_blocked(writer_pid);
        COMMIT;
        IF NOT EXISTS (SELECT FROM public.dblink_get_result('writer') AS r(id uuid) WHERE id IS NULL) THEN
            RAISE EXCEPTION 'queued append crossed the committed deletion fence (account=%)', c.account_scope;
        END IF;
        PERFORM FROM public.dblink_get_result('writer') AS r(id uuid); -- drain before reusing the connection
        IF (SELECT array_agg(e.track_id) FROM public.playlist_tracks e JOIN public.playlists p ON p.id = e.playlist_id
            WHERE p.public_id = c.handle) IS DISTINCT FROM ARRAY[c.admitted] THEN
            RAISE EXCEPTION 'fenced playlist must retain only the pre-fence append';
        END IF;
    END LOOP;
END;
$$;
SELECT public.dblink_disconnect('writer');
-- Restore the synthetic second owner for the Rust store's cross-owner checks.
UPDATE public.users SET deleting_at = NULL WHERE id = 'fixture-user-other';
\echo Private playlist rehearsal: upgrade, owner CRUD, concurrent fences and rollback passed
