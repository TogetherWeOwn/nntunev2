-- Executable persistence rehearsal, not HTTP handlers or deployed functions.
-- `owner` must come from the authenticated principal, never the request body.
CREATE SCHEMA private_playlist_rehearsal;
SET LOCAL search_path = private_playlist_rehearsal, pg_catalog, pg_temp;

CREATE FUNCTION create_playlist(owner text, handle text, title text) RETURNS text
LANGUAGE sql AS $$
    INSERT INTO public.playlists (owner_user_id, public_id, name, created_at)
    SELECT u.id, handle, btrim(title), extract(epoch FROM now())::bigint
    FROM public.users u WHERE u.id = owner AND u.deleting_at IS NULL
        AND char_length(btrim(title)) BETWEEN 1 AND 120
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
        SELECT jsonb_agg(jsonb_build_object('id', e.id, 'track_id', e.track_id,
            'position', e.position) ORDER BY e.position)
        FROM public.playlist_tracks e WHERE e.playlist_id = p.id
    ), '[]'::jsonb)
    FROM owned_playlists(owner) p WHERE p.public_id = handle;
$$;

-- Lock in user -> playlist order. The append runs its position/duplicate
-- query in a separate statement after the lock (fresh READ COMMITTED snapshot).
CREATE FUNCTION lock_playlist(owner text, handle text, include_deleting boolean DEFAULT false)
RETURNS bigint LANGUAGE plpgsql AS $$
DECLARE target bigint;
BEGIN
    PERFORM 1 FROM public.users u WHERE u.id = owner AND u.deleting_at IS NULL FOR SHARE;
    IF NOT FOUND THEN RETURN NULL; END IF;
    SELECT p.id INTO target FROM public.playlists p
    WHERE p.owner_user_id = owner AND p.public_id = handle AND NOT p.is_public
        AND (include_deleting OR p.deleting_at IS NULL)
    FOR UPDATE;
    RETURN target;
END;
$$;

CREATE FUNCTION rename_playlist(owner text, handle text, title text) RETURNS text
LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle);
BEGIN
    UPDATE public.playlists p SET name = btrim(title)
    WHERE p.id = target AND p.owner_user_id = owner
        AND char_length(btrim(title)) BETWEEN 1 AND 120;
    IF FOUND THEN RETURN handle; END IF;
    RETURN NULL;
END;
$$;

CREATE FUNCTION add_track(owner text, handle text, track uuid) RETURNS uuid
LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle); entry uuid;
BEGIN
    -- Synthetic stand-in for the Library membership predicate. Never use
    -- track existence alone, or this temporary relation, in a live adapter.
    INSERT INTO public.playlist_tracks (playlist_id, track_id, position)
    SELECT p.id, track, next.position
    FROM public.playlists p
    CROSS JOIN LATERAL (
        SELECT greatest(jsonb_array_length(p.items), max(e.position) + 1) AS position
        FROM public.playlist_tracks e WHERE e.playlist_id = p.id
    ) next
    WHERE p.id = target AND p.owner_user_id = owner AND next.position < 500
        AND EXISTS (SELECT 1 FROM pg_temp.member_tracks m WHERE m.owner_id = owner AND m.track_id = track)
        AND NOT EXISTS (SELECT 1 FROM public.playlist_tracks e WHERE e.playlist_id = p.id AND e.track_id = track)
    ON CONFLICT (playlist_id, track_id) WHERE NOT is_legacy DO NOTHING
    RETURNING id INTO entry;
    RETURN entry;
END;
$$;

-- Request fenced deletion; physical removal and cleanup belong to the
-- cutover's existing ADR delete-operation protocol, not a bare DELETE/FK cascade.
CREATE FUNCTION delete_playlist(owner text, handle text) RETURNS uuid LANGUAGE plpgsql AS $$
DECLARE target bigint := lock_playlist(owner, handle, true); operation uuid;
BEGIN
    IF target IS NULL THEN RETURN NULL; END IF;
    SELECT d.id INTO operation FROM public.delete_operations d
    WHERE d.user_id = owner AND d.scope = 'playlist' AND d.container_id = handle;
    IF FOUND THEN RETURN operation; END IF;
    UPDATE public.playlists p SET deleting_at = now() WHERE p.id = target AND p.owner_user_id = owner;
    INSERT INTO public.delete_operations (user_id, scope, container_id, started_at)
    VALUES (owner, 'playlist', handle, now()) RETURNING id INTO operation;
    RETURN operation;
END;
$$;
