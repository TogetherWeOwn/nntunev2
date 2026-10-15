-- NNTune v2 migration 0003: genre + playlist library.
--
-- PlanetScale Postgres via Hyperdrive. All access uses the parameterized
-- statements in workers/api/src/library.rs (never string-interpolated).
--
-- Identity discipline: `id` is the internal row key and is NEVER returned
-- to clients. `public_id` is the opaque handle exposed as `id` in API
-- responses. `owner_user_id` filters server-side in WHERE clauses and is
-- never in a SELECT list, so responses cannot leak ownership — except
-- inside the `(owner_user_id = $2)` boolean of the mutation-lookup
-- statements, which answers the 403-vs-404 question without exposing the
-- id itself.
--
-- Lists are keyset pages (`ORDER BY id DESC LIMIT $2` with an opaque
-- cursor over the public id): insertion order is the newest-first key,
-- immune to timestamp skew, and the primary key serves that ordering
-- directly — no extra index. The owner/visibility indexes below serve
-- the page filters.

CREATE TABLE IF NOT EXISTS genres (
    id              BIGSERIAL PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    owner_user_id   TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    is_public       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_genres_owner ON genres (owner_user_id);
CREATE INDEX IF NOT EXISTS idx_genres_public ON genres (is_public) WHERE is_public = TRUE;

CREATE TABLE IF NOT EXISTS playlists (
    id              BIGSERIAL PRIMARY KEY,
    public_id       TEXT NOT NULL UNIQUE,
    owner_user_id   TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    -- Opaque track references as a JSON array of strings. No foreign key:
    -- the catalog mirror owns track identity; the API bounds count/length.
    items           JSONB NOT NULL DEFAULT '[]',
    is_public       BOOLEAN NOT NULL DEFAULT FALSE,
    created_at      BIGINT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_playlists_owner ON playlists (owner_user_id);
CREATE INDEX IF NOT EXISTS idx_playlists_public ON playlists (is_public) WHERE is_public = TRUE;
