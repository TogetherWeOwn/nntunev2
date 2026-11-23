-- $1: a single invite row ID. Returns its ID only when an audit was written;
-- zero rows does not distinguish ineligible from previously observed.
WITH locked_invite AS MATERIALIZED (
    SELECT id, issued_by, kind, window_id, expires_at
    FROM public.invite_codes
    WHERE id = $1 AND NOT used AND redeemed_at IS NULL AND revoked_at IS NULL
      AND expiry_observed_at IS NULL
    FOR NO KEY UPDATE
), locked_bounds AS MATERIALIZED (
    -- Same lock order as claim; FOR SHARE conflicts with an early close.
    SELECT i.*, w.starts_at, w.ends_at
    FROM locked_invite i
    LEFT JOIN LATERAL (
        SELECT starts_at, ends_at FROM public.invite_windows
        WHERE id = i.window_id AND i.kind = 'member'
        FOR SHARE
    ) w ON TRUE
), checked AS MATERIALIZED (
    SELECT b.*, floor(extract(epoch FROM clock_timestamp()))::bigint AS observed_at
    FROM locked_bounds b
), audited AS (
    INSERT INTO public.audit_log (user_id, action, ok, source_ip, at, details)
    SELECT COALESCE(issued_by, 'system'), 'invite.expired', TRUE, '', observed_at,
           jsonb_build_object('invite_id', id)
    FROM checked
    WHERE (kind = 'admin' AND expires_at <= observed_at)
       OR (kind = 'member' AND starts_at <= observed_at
           AND LEAST(expires_at, ends_at) <= observed_at)
    RETURNING at
)
-- No marker without an inserted audit; a statement error rolls back both.
UPDATE public.invite_codes i SET expiry_observed_at = a.at
FROM audited a WHERE i.id = $1
RETURNING i.id
