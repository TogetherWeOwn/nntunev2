-- Regression test for the playback-enforcement statements: the lease
-- acquire/touch upserts from workers/api/src/playback.rs, the ever-paid
-- probe from billing.rs, and the plans-row caps read from
-- plan_limits.rs — mirrored here with literal values. Run after ALL
-- migrations (see ../run_migration_tests.sh) in one rolled-back
-- transaction; any broken expectation RAISEs and fails the run.
--
-- Scope: first acquire lands at epoch 1; same-session re-acquire
-- replaces the track and bumps the epoch; another session facing a
-- live lease gets zero rows (the 429); takeover and post-expiry
-- acquires bump the epoch; the touch extends only the exact holder
-- and never shortens a stored deadline (out-of-order heartbeats);
-- the acquire stamps an immutable per-play continuation deadline that
-- clamps the lease and lets an expired deadline be taken over; the
-- touch refuses abandoned (expires_at <= now) or past-deadline rows and
-- clamps to the deadline; both writers refuse a deleting account through the locking
-- writer fence; ever-paid distinguishes
-- paid-once from never-paid.

BEGIN;

INSERT INTO users (id, email, created_at)
VALUES ('pls-user', 'pls-user@example.com', 1000),
       ('pls-never', 'pls-never@example.com', 1000),
       ('pls-lapsed', 'pls-lapsed@example.com', 1000);

-- First acquire: epoch 1.
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-1', 'track-1', 'play-1', 1, LEAST(2000, 5000), 5000, 1955 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 1955 OR playback_leases.continuation_until <= 1955 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_got1 FROM got;

-- Same session, new track (queue advance): replaces, epoch 2.
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-1', 'track-2', 'play-2', 1, LEAST(2010, 5000), 5000, 1965 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 1965 OR playback_leases.continuation_until <= 1965 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_got2 FROM got;

-- Another session against the live lease, no takeover: zero rows.
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-2', 'track-3', 'play-3', 1, LEAST(2020, 5000), 5000, 1975 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 1975 OR playback_leases.continuation_until <= 1975 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT count(*) AS n INTO TEMP TABLE pls_got3 FROM got;

-- Same other session WITH takeover: epoch 3, holder changes.
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-2', 'track-3', 'play-3', 1, LEAST(2020, 5000), 5000, 1975 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 1975 OR playback_leases.continuation_until <= 1975 OR playback_leases.session_id = EXCLUDED.session_id OR TRUE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_got4 FROM got;

-- Touch by the displaced holder (stale epoch): zero rows.
WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 2100)), updated_at = GREATEST(playback_leases.updated_at, 1980)
    FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-user' AND session_id = 'sess-1' AND play_id = 'play-2' AND epoch = 2 AND playback_leases.expires_at > 1980 AND playback_leases.continuation_until > 1980
    RETURNING expires_at
)
SELECT count(*) AS n INTO TEMP TABLE pls_touch_stale FROM touched;

-- Touch by the current holder: extends.
WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 2100)), updated_at = GREATEST(playback_leases.updated_at, 1980)
    FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-user' AND session_id = 'sess-2' AND play_id = 'play-3' AND epoch = 3 AND playback_leases.expires_at > 1980 AND playback_leases.continuation_until > 1980
    RETURNING expires_at
)
SELECT expires_at INTO TEMP TABLE pls_touch_ok FROM touched;

-- Out-of-order touch by the current holder (older deadline 2050,
-- arriving after the 2100 write): still matches the holder, but the
-- stored deadline must not shorten.
WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 2050)), updated_at = GREATEST(playback_leases.updated_at, 1970)
    FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-user' AND session_id = 'sess-2' AND play_id = 'play-3' AND epoch = 3 AND playback_leases.expires_at > 1970 AND playback_leases.continuation_until > 1970
    RETURNING expires_at
)
SELECT expires_at INTO TEMP TABLE pls_touch_reordered FROM touched;

-- After expiry (now past 2100) a third session acquires: epoch 4.
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-3', 'track-4', 'play-4', 1, LEAST(2200, 5000), 5000, 2155 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 2155 OR playback_leases.continuation_until <= 2155 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_got5 FROM got;

-- Deadline-bound fixtures: a separate user whose play carries a short
-- deadline (acquire at 3000, lease request 3045, deadline 3060).
INSERT INTO users (id, email, created_at)
VALUES ('pls-deadline', 'pls-deadline@example.com', 1000);

WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-deadline', 'sess-a', 'track-a', 'play-a', 1, LEAST(3045, 3060), 3060, 3000 FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at WHERE playback_leases.expires_at <= 3000 OR playback_leases.continuation_until <= 3000 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_dl_acquire FROM got;

-- Touch near the deadline: the extension clamps to the deadline.
WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 3090)), updated_at = GREATEST(playback_leases.updated_at, 3040)
    FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-deadline' AND session_id = 'sess-a' AND play_id = 'play-a' AND epoch = 1 AND playback_leases.expires_at > 3040 AND playback_leases.continuation_until > 3040
    RETURNING expires_at
)
SELECT expires_at INTO TEMP TABLE pls_dl_clamped FROM touched;

-- Touch at the lease-expiry-equality instant (3060 == expires_at): refused.
WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 3100)), updated_at = GREATEST(playback_leases.updated_at, 3060)
    FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-deadline' AND session_id = 'sess-a' AND play_id = 'play-a' AND epoch = 1 AND playback_leases.expires_at > 3060 AND playback_leases.continuation_until > 3060
    RETURNING expires_at
)
SELECT count(*) AS n INTO TEMP TABLE pls_dl_equal FROM touched;

-- Another session once the deadline has passed takes over without the
-- takeover flag (the lease row itself is also at its clamp).
WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-deadline', 'sess-b', 'track-b', 'play-b', 1, LEAST(3105, 3160), 3160, 3061 FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at WHERE playback_leases.expires_at <= 3061 OR playback_leases.continuation_until <= 3061 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_dl_takeover FROM got;

-- Legacy row (deadline 0, lease still fresh): a touch is refused and a
-- different session may acquire immediately (fail closed).
UPDATE playback_leases SET continuation_until = 0, expires_at = 9000
WHERE user_id = 'pls-deadline';

WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 9100)), updated_at = GREATEST(playback_leases.updated_at, 3070)
    FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-deadline' AND session_id = 'sess-b' AND play_id = 'play-b' AND epoch = 2 AND playback_leases.expires_at > 3070 AND playback_leases.continuation_until > 3070
    RETURNING expires_at
)
SELECT count(*) AS n INTO TEMP TABLE pls_dl_legacy_touch FROM touched;

WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-deadline', 'sess-c', 'track-c', 'play-c', 1, LEAST(3115, 3200), 3200, 3071 FROM (SELECT id FROM users WHERE id = 'pls-deadline' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at WHERE playback_leases.expires_at <= 3071 OR playback_leases.continuation_until <= 3071 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT epoch INTO TEMP TABLE pls_dl_legacy_acquire FROM got;

-- Ever-paid: an incomplete (never paid) row reads false; a canceled
-- row with a paid end reads true even though it left the live slot.
INSERT INTO subscriptions
    (user_id, stripe_subscription_id, stripe_customer_id,
     stripe_subscription_item_id, stripe_price_id, plan_id, status,
     paid_through_at)
VALUES ('pls-never', 'sub_pls_never', 'cus_pls_never', 'si_pls_never',
        'price_pls', 'plus', 'incomplete', NULL),
       ('pls-lapsed', 'sub_pls_lapsed', 'cus_pls_lapsed', 'si_pls_lapsed',
        'price_pls', 'plus', 'canceled', TIMESTAMPTZ '2026-01-01 00:00:00+00');

-- Deletion fence: once the account is deleting, both writers refuse
-- (zero rows, stored lease unchanged) even for an otherwise-valid
-- post-expiry acquire and the exact holder's touch.
UPDATE users SET deleting_at = TIMESTAMPTZ '2026-01-01 00:00:00+00'
WHERE id = 'pls-user';

WITH got AS (
    INSERT INTO playback_leases (user_id, session_id, track_id, play_id, epoch, expires_at, continuation_until, updated_at)
    SELECT 'pls-user', 'sess-4', 'track-5', 'play-5', 1, LEAST(2300, 5000), 5000, 2255 FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    ON CONFLICT (user_id) DO UPDATE SET session_id = EXCLUDED.session_id, track_id = EXCLUDED.track_id, play_id = EXCLUDED.play_id, epoch = playback_leases.epoch + 1, expires_at = EXCLUDED.expires_at, continuation_until = EXCLUDED.continuation_until, updated_at = EXCLUDED.updated_at
    WHERE playback_leases.expires_at <= 2255 OR playback_leases.continuation_until <= 2255 OR playback_leases.session_id = EXCLUDED.session_id OR FALSE
    RETURNING epoch
)
SELECT count(*) AS n INTO TEMP TABLE pls_acquire_deleting FROM got;

WITH touched AS (
    UPDATE playback_leases SET expires_at = LEAST(playback_leases.continuation_until, GREATEST(playback_leases.expires_at, 2400)), updated_at = GREATEST(playback_leases.updated_at, 2260)
    FROM (SELECT id FROM users WHERE id = 'pls-user' AND deleting_at IS NULL FOR SHARE) AS writer_fence
    WHERE user_id = 'pls-user' AND session_id = 'sess-3' AND play_id = 'play-4' AND epoch = 4 AND playback_leases.expires_at > 2260 AND playback_leases.continuation_until > 2260
    RETURNING expires_at
)
SELECT count(*) AS n INTO TEMP TABLE pls_touch_deleting FROM touched;

DO $$
BEGIN
    IF (SELECT epoch FROM pls_got1) <> 1 THEN
        RAISE EXCEPTION 'first acquire must land at epoch 1';
    END IF;
    IF (SELECT epoch FROM pls_got2) <> 2 THEN
        RAISE EXCEPTION 'same-session re-acquire must bump the epoch';
    END IF;
    IF (SELECT n FROM pls_got3) <> 0 THEN
        RAISE EXCEPTION 'live lease must refuse another session (zero rows)';
    END IF;
    IF (SELECT epoch FROM pls_got4) <> 3 THEN
        RAISE EXCEPTION 'takeover must bump the epoch';
    END IF;
    IF (SELECT n FROM pls_touch_stale) <> 0 THEN
        RAISE EXCEPTION 'displaced holder touch must not extend';
    END IF;
    IF (SELECT expires_at FROM pls_touch_ok) <> 2100 THEN
        RAISE EXCEPTION 'holder touch must extend the lease';
    END IF;
    IF (SELECT expires_at FROM pls_touch_reordered) <> 2100 THEN
        RAISE EXCEPTION 'out-of-order touch must not shorten the stored deadline';
    END IF;
    IF (SELECT n FROM pls_acquire_deleting) <> 0 THEN
        RAISE EXCEPTION 'acquire must refuse a deleting account (zero rows)';
    END IF;
    IF (SELECT n FROM pls_touch_deleting) <> 0 THEN
        RAISE EXCEPTION 'touch must refuse a deleting account (zero rows)';
    END IF;
    IF (SELECT epoch FROM pls_got5) <> 4 THEN
        RAISE EXCEPTION 'post-expiry acquire must bump the epoch';
    END IF;
    IF (SELECT session_id || ':' || play_id FROM playback_leases
        WHERE user_id = 'pls-user') <> 'sess-3:play-4' THEN
        RAISE EXCEPTION 'final holder must be the post-expiry acquirer';
    END IF;

    IF (SELECT epoch FROM pls_dl_acquire) <> 1 THEN
        RAISE EXCEPTION 'deadline fixture first acquire must land at epoch 1';
    END IF;
    IF (SELECT expires_at FROM pls_dl_clamped) <> 3060 THEN
        RAISE EXCEPTION 'touch must clamp the lease to the continuation deadline';
    END IF;
    IF (SELECT n FROM pls_dl_equal) <> 0 THEN
        RAISE EXCEPTION 'touch at the expiry/deadline instant must be refused';
    END IF;
    IF (SELECT epoch FROM pls_dl_takeover) <> 2 THEN
        RAISE EXCEPTION 'passed deadline must permit takeover by another session';
    END IF;
    IF (SELECT n FROM pls_dl_legacy_touch) <> 0 THEN
        RAISE EXCEPTION 'legacy zero-deadline row must refuse touches';
    END IF;
    IF (SELECT epoch FROM pls_dl_legacy_acquire) <> 3 THEN
        RAISE EXCEPTION 'legacy zero-deadline row must be acquirable by another session';
    END IF;
    IF (SELECT continuation_until FROM playback_leases WHERE user_id = 'pls-deadline') <> 3200 THEN
        RAISE EXCEPTION 'acquire must overwrite the continuation deadline';
    END IF;

    IF (SELECT EXISTS(SELECT 1 FROM subscriptions
            WHERE user_id = 'pls-never' AND paid_through_at IS NOT NULL)) THEN
        RAISE EXCEPTION 'never-paid account must read ever_paid = false';
    END IF;
    IF NOT (SELECT EXISTS(SELECT 1 FROM subscriptions
            WHERE user_id = 'pls-lapsed' AND paid_through_at IS NOT NULL)) THEN
        RAISE EXCEPTION 'canceled-after-paying account must read ever_paid = true';
    END IF;

    -- Plans-row caps read: the seeded plus row carries 100 h / 1 stream.
    IF (SELECT stream_hours_month || ':' || max_concurrent_streams
        FROM plans WHERE id = 'plus') <> '100:1' THEN
        RAISE EXCEPTION 'plus caps row must read 100:1';
    END IF;
    IF EXISTS (SELECT 1 FROM plans WHERE id = 'no-such-plan') THEN
        RAISE EXCEPTION 'unknown plan id must have no caps row';
    END IF;
END $$;

ROLLBACK;
