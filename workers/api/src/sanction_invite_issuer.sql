-- $1: issuer ID, $2: authorized actor ID, $3: sanitized source IP.
-- Execute only after lock_invite_issuer_for_sanction.sql returned a row in the
-- same READ COMMITTED transaction. A separate statement takes a new snapshot
-- so invites committed by a writer that held the issuer lock are visible.
-- Returns one row with the number revoked (possibly zero) on transition only.
WITH locked_invites AS MATERIALIZED (
    SELECT id, kind, window_id, expires_at
    FROM public.invite_codes
    WHERE issued_by = $1 AND NOT used AND redeemed_at IS NULL AND revoked_at IS NULL
      AND expires_at > floor(extract(epoch FROM statement_timestamp()))::bigint
    ORDER BY id
    FOR NO KEY UPDATE
), window_ids AS MATERIALIZED (
    -- Drain all invite locks before taking any window lock. A competing live
    -- claim locks its invite before taking SHARE on its window.
    SELECT array_agg(DISTINCT window_id) AS ids FROM locked_invites WHERE kind = 'member'
), locked_windows AS MATERIALIZED (
    SELECT w.* FROM window_ids k CROSS JOIN LATERAL (
        SELECT id, starts_at, ends_at FROM public.invite_windows
        WHERE id = ANY(k.ids) ORDER BY id FOR SHARE
    ) w
), cutoff AS MATERIALIZED (
    -- Use one wall-clock cutoff after the locking reads, not request time.
    SELECT floor(extract(epoch FROM clock_timestamp()))::bigint AS at,
           count(*) AS window_count FROM locked_windows
), restricted AS (
    UPDATE public.users u SET invite_issuance_restricted_at = c.at
    FROM cutoff c WHERE u.id = $1 AND u.invite_issuance_restricted_at IS NULL
    RETURNING u.invite_issuance_restricted_at AS at
), revoked AS (
    UPDATE public.invite_codes i SET revoked_at = r.at, revoked_by = $2
    FROM restricted r CROSS JOIN locked_invites l CROSS JOIN cutoff c
    LEFT JOIN locked_windows w ON w.id = l.window_id
    WHERE i.id = l.id AND l.expires_at > r.at
      AND (l.kind = 'admin' OR (c.window_count > 0 AND w.starts_at <= r.at AND r.at < w.ends_at))
    RETURNING i.id, i.revoked_at
), audited AS (
    INSERT INTO public.audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
    SELECT $2, 'invite.issuer_restricted', TRUE, $3, at, $1, NULL::jsonb FROM restricted
    UNION ALL
    SELECT $2, 'invite.revoked', TRUE, $3, revoked_at, $1, jsonb_build_object('invite_id', id) FROM revoked
    RETURNING action
)
SELECT count(*) FILTER (WHERE action = 'invite.revoked') AS revoked_count
FROM audited HAVING count(*) > 0
