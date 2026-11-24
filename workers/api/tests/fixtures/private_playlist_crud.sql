-- Executable persistence rehearsal, not HTTP handlers or deployed functions.
-- `owner` must come from the authenticated principal, never the request body.
CREATE SCHEMA private_playlist_rehearsal;
SET LOCAL search_path = private_playlist_rehearsal, pg_catalog, pg_temp;
CREATE TABLE marker AS SELECT gen_random_uuid()::text AS token;
CREATE VIEW member_tracks AS
SELECT l.user_id AS owner_id, lt.track_id FROM public.libraries l
JOIN public.library_tracks lt ON lt.library_id = l.id WHERE l.deleting_at IS NULL;

-- Observe a real lock wait before committing the fixture transaction.
CREATE FUNCTION await_blocked(writer_pid integer) RETURNS void LANGUAGE plpgsql AS $$
DECLARE deadline timestamptz := clock_timestamp() + interval '3 seconds';
BEGIN
    WHILE NOT pg_backend_pid() = ANY(pg_blocking_pids(writer_pid)) LOOP
        IF clock_timestamp() >= deadline THEN RAISE EXCEPTION 'late writer did not wait'; END IF;
        PERFORM pg_sleep(0.01);
    END LOOP;
END;
$$;

-- Simulate the executor's persisted completion state in the disposable clone.
CREATE FUNCTION complete_playlist_delete(owner text, handle text) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM public.playlist_tracks e USING public.playlists p
    WHERE e.playlist_id = p.id AND p.owner_user_id = owner AND p.public_id = handle;
    DELETE FROM public.playlists WHERE owner_user_id = owner AND public_id = handle;
    IF NOT FOUND THEN RAISE EXCEPTION 'playlist was not physically removed'; END IF;
    UPDATE public.delete_operations SET state = 'done'
    WHERE user_id = owner AND scope = 'playlist' AND container_id = handle;
    IF NOT FOUND THEN RAISE EXCEPTION 'durable operation was missing'; END IF;
END;
$$;

-- Match library::check_name's Rust Unicode White_Space trim, regardless of locale.
CREATE FUNCTION trim_name(title text) RETURNS text LANGUAGE sql IMMUTABLE AS $$
    SELECT btrim(title, U&'\0009\000A\000B\000C\000D\0020\0085\00A0\1680\2000\2001\2002\2003\2004\2005\2006\2007\2008\2009\200A\2028\2029\202F\205F\3000');
$$;
CREATE FUNCTION create_playlist(owner text, handle text, title text) RETURNS text
LANGUAGE sql AS $$
    INSERT INTO public.playlists (owner_user_id, public_id, name, created_at)
    SELECT u.id, handle, trim_name(title), extract(epoch FROM now())::bigint
    FROM public.users u WHERE u.id = owner AND u.deleting_at IS NULL
        AND char_length(trim_name(title)) BETWEEN 1 AND 120
        AND handle ~ '^[A-Za-z0-9_-]{22}$'
    FOR SHARE OF u
    RETURNING public_id;
$$;
CREATE FUNCTION owned_playlists(owner text) RETURNS SETOF public.playlists
LANGUAGE sql STABLE AS $$
    SELECT p.* FROM public.playlists p JOIN public.users u ON u.id = p.owner_user_id
    WHERE p.owner_user_id = owner AND u.deleting_at IS NULL
        AND NOT p.is_public AND p.deleting_at IS NULL;
$$;
CREATE FUNCTION list_playlists(owner text, after_id text, page_size integer)
RETURNS TABLE (id text, name text) LANGUAGE sql STABLE AS $$
    SELECT p.public_id, p.name FROM owned_playlists(owner) p
    WHERE after_id IS NULL OR p.public_id COLLATE "C" < after_id COLLATE "C"
    ORDER BY p.public_id COLLATE "C" DESC LIMIT least(greatest(page_size, 1), 50);
$$;
CREATE FUNCTION read_playlist(owner text, handle text)
RETURNS TABLE (id text, name text, legacy_items jsonb, entries jsonb) LANGUAGE sql STABLE AS $$
    SELECT p.public_id, p.name, p.items, coalesce((
        SELECT jsonb_agg(jsonb_build_object('id', e.id, 'position', e.position) ||
            CASE WHEN m.track_id IS NULL THEN '{"dormant":true}'::jsonb
                ELSE jsonb_build_object('track_id', e.track_id) END ORDER BY e.position)
        FROM public.playlist_tracks e LEFT JOIN member_tracks m ON m.owner_id = owner AND m.track_id = e.track_id
        WHERE e.playlist_id = p.id
    ), '[]'::jsonb)
    FROM owned_playlists(owner) p WHERE p.public_id = handle;
$$;
-- Lock user then playlist before the append takes a fresh READ COMMITTED snapshot.
CREATE FUNCTION lock_playlist(owner text, handle text, include_deleting boolean DEFAULT false)
RETURNS bigint LANGUAGE plpgsql AS $$
BEGIN
    PERFORM 1 FROM public.users u WHERE u.id = owner AND u.deleting_at IS NULL FOR SHARE;
    IF NOT FOUND THEN RETURN NULL; END IF;
    RETURN (SELECT p.id FROM public.playlists p
        WHERE p.owner_user_id = owner AND p.public_id = handle AND NOT p.is_public
            AND (include_deleting OR p.deleting_at IS NULL) FOR UPDATE);
END;
$$;
CREATE FUNCTION rename_playlist(owner text, handle text, title text) RETURNS text
LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle);
BEGIN
    UPDATE public.playlists p SET name = trim_name(title)
    WHERE p.id = target AND p.owner_user_id = owner
        AND char_length(trim_name(title)) BETWEEN 1 AND 120;
    IF FOUND THEN RETURN handle; END IF;
    RETURN NULL;
END;
$$;
CREATE FUNCTION add_track(owner text, handle text, track uuid) RETURNS uuid
LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle); entry uuid;
BEGIN
    -- Deduplicate under the playlist lock and require Library membership.
    INSERT INTO public.playlist_tracks (playlist_id, track_id, position)
    SELECT p.id, track, next.position
    FROM public.playlists p
    CROSS JOIN LATERAL (
        SELECT greatest(jsonb_array_length(p.items), max(e.position) + 1) AS position
        FROM public.playlist_tracks e WHERE e.playlist_id = p.id
    ) next
    WHERE p.id = target AND p.owner_user_id = owner AND next.position < 500
        AND EXISTS (SELECT 1 FROM member_tracks m WHERE m.owner_id = owner AND m.track_id = track)
        AND NOT EXISTS (SELECT 1 FROM public.playlist_tracks e WHERE e.playlist_id = p.id AND e.track_id = track)
    RETURNING id INTO entry;
    RETURN entry;
END;
$$;
-- Request fenced deletion; physical removal and cleanup belong to the
-- cutover's existing ADR delete-operation protocol, not a bare DELETE/FK cascade.
CREATE FUNCTION delete_playlist(owner text, handle text) RETURNS uuid LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle, true); operation uuid;
BEGIN
    -- The lookup also needs an active, locked owner when the container is gone.
    PERFORM 1 FROM public.users u WHERE u.id = owner AND u.deleting_at IS NULL FOR SHARE;
    IF NOT FOUND THEN RETURN NULL; END IF;
    SELECT d.id INTO operation FROM public.delete_operations d
    WHERE d.user_id = owner AND d.scope = 'playlist' AND d.container_id = handle;
    IF FOUND THEN RETURN operation; END IF;
    IF target IS NULL THEN RETURN NULL; END IF;
    UPDATE public.playlists p SET deleting_at = now() WHERE p.id = target AND p.owner_user_id = owner;
    INSERT INTO public.delete_operations (user_id, scope, container_id, started_at)
    VALUES (owner, 'playlist', handle, now()) RETURNING id INTO operation;
    RETURN operation;
END;
$$;
