-- Regression test for PgStore::insert_magic: issuing a magic link prunes the
-- same user's expired rows and nothing else. Mirrors the bound statement
-- with literal values. Runs in one transaction that is rolled back.

BEGIN;

INSERT INTO users (id, email, created_at)
VALUES ('mlr-live', 'mlr-live@example.com', 1000),
       ('mlr-other', 'mlr-other@example.com', 1000);

INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used)
VALUES ('mlr-live-expired', 'mlr-live', 'mlr-live@example.com', 1000, 1900, FALSE),
       ('mlr-live-spent-expired', 'mlr-live', 'mlr-live@example.com', 1000, 1900, TRUE),
       ('mlr-other-unexpired', 'mlr-other', 'mlr-other@example.com', 1000, 99999, FALSE);

WITH pruned AS (
    DELETE FROM magic_link_tokens WHERE user_id = 'mlr-live' AND expires_at <= 2000
)
INSERT INTO magic_link_tokens (token_hash, user_id, email, issued_at, expires_at, used)
VALUES ('mlr-live-new', 'mlr-live', 'mlr-live@example.com', 2000, 2900, FALSE);

DO $$
BEGIN
    IF (SELECT COUNT(*) FROM magic_link_tokens WHERE user_id = 'mlr-live') <> 1 THEN
        RAISE EXCEPTION 'expired magic rows of the issuing user were not pruned';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM magic_link_tokens WHERE token_hash = 'mlr-live-new') THEN
        RAISE EXCEPTION 'new magic row was not inserted';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM magic_link_tokens WHERE token_hash = 'mlr-other-unexpired') THEN
        RAISE EXCEPTION 'unexpired magic row of another user was pruned';
    END IF;
END
$$;

ROLLBACK;
