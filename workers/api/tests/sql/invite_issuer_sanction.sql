BEGIN ISOLATION LEVEL READ COMMITTED;
CREATE FUNCTION pg_temp.lock_issuer(text) RETURNS text
LANGUAGE sql VOLATILE AS :'lock_invite_issuer_sql';
CREATE FUNCTION pg_temp.apply_sanction(text, text, text) RETURNS bigint
LANGUAGE sql VOLATILE AS :'sanction_invite_issuer_sql';
CREATE FUNCTION pg_temp.sanction(issuer text) RETURNS bigint LANGUAGE plpgsql VOLATILE AS $$
BEGIN
    IF pg_temp.lock_issuer(issuer) IS NULL THEN
        RETURN NULL;
    END IF;
    RETURN pg_temp.apply_sanction(issuer, 'sanction-actor', '');
END $$;

DO $$
DECLARE
    t bigint := floor(extract(epoch FROM clock_timestamp()))::bigint;
    issuer CONSTANT text := 'sanction-target';
    actor CONSTANT text := 'sanction-actor';
    invitee CONSTANT text := 'sanction-invitee';
    closed bigint;
    open_window bigint;
    future bigint;
    restriction bigint;
    expected_ids bigint[];
    before_rows jsonb;
    before_child jsonb;
BEGIN
    INSERT INTO public.users (id, email, created_at)
    SELECT u, u || '@example.invalid', t - 10000 FROM unnest(ARRAY[issuer, actor, invitee]) AS x(u);
    UPDATE public.users SET invited_by = issuer WHERE id = invitee;
    SELECT to_jsonb(u) INTO before_child FROM public.users u WHERE id = invitee;

    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t - 3600, t) RETURNING id INTO closed;
    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t, t + 3600) RETURNING id INTO open_window;
    INSERT INTO public.invite_windows (starts_at, ends_at)
    VALUES (t + 3600, t + 7200) RETURNING id INTO future;

    INSERT INTO public.invite_codes
        (code_hash, issued_by, kind, window_id, created_at, expires_at, used, redeemed_at, redeemed_by, revoked_at)
    SELECT 'sanction-fixture-' || label, owner, CASE WHEN w IS NULL THEN 'admin' ELSE 'member' END,
           w, t - 3600, deadline, used, redeemed, redeemer, revoked
    FROM (VALUES
        ('admin',       issuer, NULL,        t + 1800, FALSE, NULL, NULL,    NULL),
        ('member',      issuer, open_window, t + 1800, FALSE, NULL, NULL,    NULL),
        ('legacy-used', issuer, NULL,        t + 1800, TRUE,  NULL, NULL,    NULL),
        ('redeemed',    issuer, NULL,        t + 1800, TRUE,  t,    invitee, NULL),
        ('stamp-only',  issuer, NULL,        t + 1800, FALSE, t,    NULL,    NULL),
        ('revoked',     issuer, NULL,        t + 1800, FALSE, NULL, NULL,    t - 1),
        ('expired',     issuer, NULL,        t,        FALSE, NULL, NULL,    NULL),
        ('closed',      issuer, closed,      t + 1800, FALSE, NULL, NULL,    NULL),
        ('future',      issuer, future,      t + 5400, FALSE, NULL, NULL,    NULL),
        ('other',       actor,  open_window, t + 1800, FALSE, NULL, NULL,    NULL),
        ('no-issuer',   NULL,   NULL,        t + 1800, FALSE, NULL, NULL,    NULL)
    ) f(label, owner, w, deadline, used, redeemed, redeemer, revoked);
    SELECT array_agg(id ORDER BY id) INTO expected_ids FROM public.invite_codes
    WHERE code_hash IN ('sanction-fixture-admin', 'sanction-fixture-member');
    SELECT jsonb_agg(to_jsonb(i) - CASE WHEN id = ANY(expected_ids)
                     THEN ARRAY['revoked_at', 'revoked_by'] ELSE ARRAY[]::text[] END ORDER BY id)
    INTO before_rows FROM public.invite_codes i;

    IF pg_temp.sanction(issuer) IS DISTINCT FROM 2::bigint THEN
        RAISE EXCEPTION 'expected exactly two revocations';
    END IF;
    SELECT invite_issuance_restricted_at INTO restriction FROM public.users WHERE id = issuer;
    IF restriction IS NULL OR restriction < t
        OR (SELECT invite_issuance_restricted_at FROM public.users WHERE id = actor) IS NOT NULL
        OR (SELECT to_jsonb(u) FROM public.users u WHERE id = invitee) IS DISTINCT FROM before_child
        OR (SELECT jsonb_agg(to_jsonb(i) - CASE WHEN id = ANY(expected_ids)
                              THEN ARRAY['revoked_at', 'revoked_by'] ELSE ARRAY[]::text[] END ORDER BY id)
            FROM public.invite_codes i) IS DISTINCT FROM before_rows
        OR (SELECT array_agg(id ORDER BY id) FROM public.invite_codes
            WHERE revoked_at = restriction AND revoked_by = actor) IS DISTINCT FROM expected_ids THEN
        RAISE EXCEPTION 'wrong sanction scope, timestamp, or historical rewrite';
    END IF;
    IF pg_temp.sanction(issuer) IS NOT NULL OR pg_temp.sanction('sanction-missing') IS NOT NULL THEN
        RAISE EXCEPTION 'retry or missing issuer reported a transition';
    END IF;
    IF (SELECT count(*) FROM public.audit_log WHERE user_id = actor) <> 3
        OR (SELECT count(*) FROM public.audit_log WHERE user_id = actor AND subject_user_id = issuer
            AND at = restriction AND ok AND source_ip = '' AND action = 'invite.issuer_restricted'
            AND details IS NULL) <> 1
        OR (SELECT array_agg((details->>'invite_id')::bigint ORDER BY (details->>'invite_id')::bigint)
            FROM public.audit_log a JOIN public.invite_codes i ON a.details = jsonb_build_object('invite_id', i.id)
            WHERE user_id = actor AND subject_user_id = issuer AND at = restriction AND ok
                AND source_ip = '' AND action = 'invite.revoked') IS DISTINCT FROM expected_ids THEN
        RAISE EXCEPTION 'wrong audit count, attribution, or payload';
    END IF;
END $$;

-- Fail specifically on the invite audit, after the write CTEs have supplied it.
ALTER TABLE public.audit_log ADD CONSTRAINT fixture_reject_sanction_revoke
CHECK (action <> 'invite.revoked' OR subject_user_id IS DISTINCT FROM 'sanction-retry') NOT VALID;
DO $$
DECLARE
    t bigint := floor(extract(epoch FROM clock_timestamp()))::bigint;
    issuer CONSTANT text := 'sanction-retry';
    invite bigint;
    fault text;
BEGIN
    INSERT INTO public.users (id, email, created_at)
    VALUES (issuer, 'sanction-retry@example.invalid', t), ('sanction-empty', 'sanction-empty@example.invalid', t);
    INSERT INTO public.invite_codes (code_hash, issued_by, created_at, expires_at)
    VALUES ('sanction-fixture-retry', issuer, t, t + 3600) RETURNING id INTO invite;
    BEGIN
        PERFORM pg_temp.sanction(issuer);
        RAISE EXCEPTION 'injected audit failure was not observed';
    EXCEPTION WHEN check_violation THEN
        GET STACKED DIAGNOSTICS fault = CONSTRAINT_NAME;
        IF fault IS DISTINCT FROM 'fixture_reject_sanction_revoke' THEN RAISE; END IF;
    END;
    IF (SELECT invite_issuance_restricted_at FROM public.users WHERE id = issuer) IS NOT NULL
        OR EXISTS (SELECT FROM public.invite_codes WHERE id = invite AND (revoked_at IS NOT NULL OR revoked_by IS NOT NULL))
        OR EXISTS (SELECT FROM public.audit_log WHERE subject_user_id = issuer) THEN
        RAISE EXCEPTION 'failed audit left partial state';
    END IF;
    ALTER TABLE public.audit_log DROP CONSTRAINT fixture_reject_sanction_revoke;
    IF pg_temp.sanction(issuer) IS DISTINCT FROM 1::bigint
        OR pg_temp.sanction('sanction-empty') IS DISTINCT FROM 0::bigint THEN
        RAISE EXCEPTION 'retry did not apply or empty issuer did not transition';
    END IF;
    IF (SELECT count(*) FROM public.audit_log WHERE subject_user_id = issuer) <> 2
        OR (SELECT count(*) FROM public.audit_log WHERE subject_user_id = 'sanction-empty'
            AND action = 'invite.issuer_restricted') <> 1 THEN
        RAISE EXCEPTION 'wrong transition audits after retry or for empty issuer';
    END IF;
END $$;
ROLLBACK;
