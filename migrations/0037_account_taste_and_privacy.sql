-- NNTune v2 migration 0037: per-account taste, private sessions, private plays.
--
-- PlanetScale Postgres via Hyperdrive. All access uses the parameterized
-- statements in workers/api/src/taste.rs (never string-interpolated).
--
-- Three tables for the library-preference slice:
--
-- 1. `account_taste`: one durable signal row per (user, kind, target). Likes
--    are `signal = 'love'`; `dislike`/`hide`/`ban`/`snooze` share the row so
--    a new signal replaces the prior state on the same target. `snooze_until`
--    is non-NULL only on snooze rows; reads filter `snooze_until <= now`.
--    `created_at`/`updated_at` are BIGINT Unix seconds matching the ledger's
--    `recorded_at` domain.
--
-- 2. `private_sessions`: an active private-listening session per
--    (user, session_id). `PUT /v1/me/private-session` upserts with a
--    server-side TTL; expired rows are simply not matched at play-grant time.
--
-- 3. `private_plays`: server-side marks for plays minted while their session
--    was private, keyed by the derived ledger idempotency key
--    (`play:{user}:{track}:{play}`). Written at `POST /v1/plays` grant
--    issuance — before any bytes are metered — so a private play is
--    suppressed from history/taste exposure from first use while billing
--    (`stream_play_ledger`) is completely unaffected. No retrofit: nothing
--    back-fills old rows and nothing un-marks a play.
--
-- Account deletion: all three FK to `users (id) ON DELETE CASCADE`, so the
-- executor's user erase removes preference data with the account. Writers
-- additionally fence `deleting_at` like every other user-owned writer.
--
-- Replay-safety (run_migration_tests.sh re-applies every migration):
-- every statement is `IF NOT EXISTS`.
--
-- Conventions (migrations/README.md): the first executable statement is the
-- `SET search_path` header so the migration is self-contained under any
-- role or runner `search_path`, and every relation reference is
-- schema-qualified (`public.…`).

SET search_path = public, pg_catalog;

CREATE TABLE IF NOT EXISTS public.account_taste (
    user_id      TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE
                 CHECK (char_length(user_id) BETWEEN 1 AND 128),
    kind         TEXT NOT NULL CHECK (kind IN ('track', 'release')),
    target_id    TEXT NOT NULL CHECK (char_length(target_id) BETWEEN 1 AND 512),
    signal       TEXT NOT NULL
                 CHECK (signal IN ('love', 'dislike', 'hide', 'ban', 'snooze')),
    snooze_until BIGINT NULL
                 CHECK (snooze_until IS NULL OR (snooze_until >= 0 AND snooze_until <= 253402300799)),
    created_at   BIGINT NOT NULL CHECK (created_at >= 0 AND created_at <= 253402300799),
    updated_at   BIGINT NOT NULL CHECK (updated_at >= 0 AND updated_at <= 253402300799),
    PRIMARY KEY (user_id, kind, target_id),
    CHECK ((signal = 'snooze') = (snooze_until IS NOT NULL))
);
CREATE INDEX IF NOT EXISTS idx_account_taste_user_signal ON public.account_taste (user_id, signal);

CREATE TABLE IF NOT EXISTS public.private_sessions (
    user_id    TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE
               CHECK (char_length(user_id) BETWEEN 1 AND 128),
    session_id TEXT NOT NULL CHECK (char_length(session_id) BETWEEN 1 AND 128),
    expires_at BIGINT NOT NULL CHECK (expires_at >= 0 AND expires_at <= 253402300799),
    PRIMARY KEY (user_id, session_id)
);
CREATE INDEX IF NOT EXISTS idx_private_sessions_expires ON public.private_sessions (expires_at);

CREATE TABLE IF NOT EXISTS public.private_plays (
    user_id   TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE
              CHECK (char_length(user_id) BETWEEN 1 AND 128),
    play_key  TEXT NOT NULL UNIQUE
              CHECK (char_length(play_key) BETWEEN 1 AND 1159)
              CHECK (play_key ~ '^play:[A-Za-z0-9_-]+:[A-Za-z0-9_-]+:[A-Za-z0-9_-]+$')
              CHECK (split_part(play_key, ':', 2) = user_id),
    marked_at BIGINT NOT NULL CHECK (marked_at >= 0 AND marked_at <= 253402300799),
    PRIMARY KEY (user_id, play_key)
);
