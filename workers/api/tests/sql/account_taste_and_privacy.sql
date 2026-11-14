-- Exercise the shipped migration in the disposable PostgreSQL replay lane.
BEGIN;
\ir ../../../../migrations/0041_account_taste_and_privacy.sql

CREATE FUNCTION pg_temp.expect_error(statement TEXT, expected TEXT) RETURNS VOID
LANGUAGE plpgsql AS $$
BEGIN
    BEGIN
        EXECUTE statement;
    EXCEPTION WHEN OTHERS THEN
        IF SQLSTATE = expected THEN RETURN; END IF;
        RAISE EXCEPTION 'expected %, got % for %', expected, SQLSTATE, statement;
    END;
    RAISE EXCEPTION 'expected % but statement succeeded: %', expected, statement;
END $$;

INSERT INTO public.users (id, email, created_at) VALUES
    ('taste-a', 'taste-a@example.com', 1), ('taste-b', 'taste-b@example.com', 1);
INSERT INTO public.account_taste VALUES
    ('taste-a', 'track', 't', 'love', NULL, 0, 253402300799),
    ('taste-a', 'release', 'r', 'snooze', 253402300799, 1, 1),
    ('taste-b', 'track', 't', 'hide', NULL, 1, 1);
INSERT INTO public.private_sessions VALUES ('taste-a', 'session', 0), ('taste-b', 'session', 253402300799);
INSERT INTO public.private_plays VALUES ('taste-a', 'play:taste-a:track:one', 0),
                                       ('taste-b', 'play:taste-b:track:one', 253402300799);

-- Each statement changes one relevant invariant; check violations cannot mask FK/unique failures.
SELECT pg_temp.expect_error(s, '23514') FROM (VALUES
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','artist','x','love',NULL,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','','love',NULL,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track',repeat('x',513),'love',NULL,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','invalid',NULL,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','snooze',NULL,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','love',1,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','snooze',-1,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','snooze',253402300800,1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','love',NULL,-1,1)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','x','love',NULL,1,253402300800)$q$),
    ($q$INSERT INTO public.account_taste VALUES ('','track','x','love',NULL,1,1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('taste-a','',1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('taste-a',repeat('x',129),1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('taste-a','x',-1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('taste-a','x',253402300800)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','',1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-a:track:bad!',1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-b:track:two',1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-a:track:' || repeat('x',1141),1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-a:track:two',-1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-a:track:two',253402300800)$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23505') FROM (VALUES
    ($q$INSERT INTO public.account_taste VALUES ('taste-a','track','t','ban',NULL,1,1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('taste-a','session',1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('taste-a','play:taste-a:track:one',1)$q$)
) AS cases(s);
SELECT pg_temp.expect_error(s, '23503') FROM (VALUES
    ($q$INSERT INTO public.account_taste VALUES ('missing','track','x','love',NULL,1,1)$q$),
    ($q$INSERT INTO public.private_sessions VALUES ('missing','x',1)$q$),
    ($q$INSERT INTO public.private_plays VALUES ('missing','play:missing:track:x',1)$q$)
) AS cases(s);

DELETE FROM public.users WHERE id = 'taste-a';
DO $$
BEGIN
    IF EXISTS (SELECT FROM public.account_taste WHERE user_id = 'taste-a') OR
       EXISTS (SELECT FROM public.private_sessions WHERE user_id = 'taste-a') OR
       EXISTS (SELECT FROM public.private_plays WHERE user_id = 'taste-a') THEN
        RAISE EXCEPTION 'erasure left private data';
    END IF;
    IF (SELECT count(*) FROM public.account_taste WHERE user_id = 'taste-b') <> 1 OR
       (SELECT count(*) FROM public.private_sessions WHERE user_id = 'taste-b') <> 1 OR
       (SELECT count(*) FROM public.private_plays WHERE user_id = 'taste-b') <> 1 THEN
        RAISE EXCEPTION 'erasure affected another owner';
    END IF;
END $$;
ROLLBACK;
