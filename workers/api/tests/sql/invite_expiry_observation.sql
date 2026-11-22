BEGIN;
CREATE FUNCTION pg_temp.observe_invite_expiry(bigint) RETURNS bigint
LANGUAGE sql VOLATILE AS :'observe_invite_expiry_sql';

-- Exercise the defensive missing-window branch with a corrupt fixture only.
ALTER TABLE public.invite_codes DROP CONSTRAINT invite_codes_window_fk;

DO $$
DECLARE
    t bigint := floor(extract(epoch FROM clock_timestamp()))::bigint;
    ended bigint;
    live bigint;
    future bigint;
    early bigint;
    retry bigint;
    actor CONSTANT text := 'fixture-observation-issuer';
    r record;
    expected boolean;
    fault text;
BEGIN
    INSERT INTO public.users (id, email, created_at)
    VALUES (actor, 'fixture-observation-issuer@example.invalid', t - 7200);
    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t - 3600, t - 1800) RETURNING id INTO ended;
    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t - 1800, t + 1800) RETURNING id INTO live;
    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t + 1800, t + 3600) RETURNING id INTO future;

    INSERT INTO public.invite_codes
        (code_hash, kind, window_id, created_at, expires_at, used, redeemed_at, revoked_at, issued_by, email)
    SELECT 'obs-fixture-' || label, CASE WHEN w IS NULL THEN 'admin' ELSE 'member' END,
           w, t - 3600, deadline, used, redeemed, revoked, issuer, 'bound@example.invalid'
    FROM (VALUES
        ('admin',        NULL,   t - 1,    FALSE, NULL,  NULL, actor),
        ('fresh',        NULL,   t + 3600, FALSE, NULL,  NULL, NULL),
        ('normal-end',   ended,  t + 3600, FALSE, NULL,  NULL, NULL),
        ('code-end',     live,   t - 1,    FALSE, NULL,  NULL, NULL),
        ('early-close',  live,   t + 3600, FALSE, NULL,  NULL, NULL),
        ('future',       future, t - 1,    FALSE, NULL,  NULL, NULL),
        ('missing',      -1,     t - 1,    FALSE, NULL,  NULL, NULL),
        ('revoked',      ended,  t - 1,    FALSE, NULL,  t,    NULL),
        ('stamped',      ended,  t - 1,    FALSE, t,     NULL, NULL),
        ('used',         NULL,   t - 1,    TRUE,  NULL,  NULL, NULL)
    ) f(label, w, deadline, used, redeemed, revoked, issuer);

    FOR r IN SELECT id, code_hash, issued_by FROM public.invite_codes WHERE code_hash LIKE 'obs-fixture-%' LOOP
        expected := r.code_hash IN ('obs-fixture-admin', 'obs-fixture-normal-end', 'obs-fixture-code-end');
        IF (pg_temp.observe_invite_expiry(r.id) IS NOT NULL) IS DISTINCT FROM expected THEN
            RAISE EXCEPTION 'unexpected expiry observation: %', r.code_hash;
        END IF;
        IF pg_temp.observe_invite_expiry(r.id) IS NOT NULL THEN
            RAISE EXCEPTION 'sequential duplicate expiry observation';
        END IF;
        IF (SELECT count(*) FROM public.audit_log a
            WHERE a.action = 'invite.expired' AND a.details->'invite_id' = to_jsonb(r.id))
           <> expected::int THEN
            RAISE EXCEPTION 'unexpected expiry audit count: %', r.code_hash;
        END IF;
        IF (SELECT expiry_observed_at IS NOT NULL FROM public.invite_codes WHERE id = r.id) <> expected THEN
            RAISE EXCEPTION 'unexpected expiry marker: %', r.code_hash;
        END IF;
        IF expected AND NOT EXISTS (
            SELECT FROM public.audit_log a JOIN public.invite_codes i ON i.id = r.id
            WHERE a.action = 'invite.expired' AND a.at = i.expiry_observed_at AND a.at >= t
              AND a.user_id = COALESCE(r.issued_by, 'system') AND a.ok AND a.subject_user_id IS NULL
              AND a.source_ip = '' AND a.details = jsonb_build_object('invite_id', i.id)
        ) THEN
            RAISE EXCEPTION 'wrong actor, time or expiry audit payload';
        END IF;
    END LOOP;
    IF pg_temp.observe_invite_expiry(-1) IS NOT NULL THEN
        RAISE EXCEPTION 'unknown row observed as expired';
    END IF;

    -- An unchanged code deadline becomes ineligible only after shortening the window.
    UPDATE public.invite_windows SET ends_at = t WHERE id = live;
    SELECT id INTO STRICT early FROM public.invite_codes WHERE code_hash = 'obs-fixture-early-close';
    IF pg_temp.observe_invite_expiry(early) IS DISTINCT FROM early THEN
        RAISE EXCEPTION 'early close was not observed';
    END IF;

    INSERT INTO public.invite_codes (code_hash, created_at, expires_at)
    VALUES ('obs-fixture-retry', t - 3600, t - 1) RETURNING id INTO retry;
    ALTER TABLE public.audit_log ADD CONSTRAINT reject_expiry_fixture CHECK (action <> 'invite.expired') NOT VALID;
    BEGIN
        PERFORM pg_temp.observe_invite_expiry(retry);
        RAISE EXCEPTION 'fault injection did not reject the audit';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS fault = CONSTRAINT_NAME;
        IF fault IS DISTINCT FROM 'reject_expiry_fixture' THEN RAISE; END IF;
    END;
    IF EXISTS (SELECT FROM public.invite_codes WHERE id = retry AND expiry_observed_at IS NOT NULL)
       OR EXISTS (SELECT FROM public.audit_log WHERE action = 'invite.expired'
                  AND details->'invite_id' = to_jsonb(retry)) THEN
        RAISE EXCEPTION 'failed write left committed expiry state';
    END IF;
    ALTER TABLE public.audit_log DROP CONSTRAINT reject_expiry_fixture;
    IF pg_temp.observe_invite_expiry(retry) IS DISTINCT FROM retry THEN
        RAISE EXCEPTION 'observation was not retryable';
    END IF;
END $$;

-- Require proof that the database clock really read exactly the deadline.
-- Retry only if the fixture crossed a wall-clock tick during the statement.
DO $$
DECLARE
    deadline bigint;
    invite bigint;
    observed bigint;
BEGIN
    FOR n IN 1..5 LOOP
        deadline := floor(extract(epoch FROM clock_timestamp()))::bigint;
        INSERT INTO public.invite_codes (code_hash, created_at, expires_at)
        VALUES ('obs-fixture-equality-' || n, deadline - 1, deadline) RETURNING id INTO invite;
        observed := pg_temp.observe_invite_expiry(invite);
        IF EXISTS (SELECT FROM public.audit_log WHERE action = 'invite.expired' AND at = deadline
                   AND details->'invite_id' = to_jsonb(invite)) THEN
            IF observed IS DISTINCT FROM invite THEN RAISE EXCEPTION 'missing equality marker'; END IF;
            RETURN;
        END IF;
        IF deadline = floor(extract(epoch FROM clock_timestamp()))::bigint THEN
            RAISE EXCEPTION 'invite must expire at equality';
        END IF;
    END LOOP;
    RAISE EXCEPTION 'could not exercise the exact database clock boundary';
END $$;
ROLLBACK;
