-- Authenticated invited member, using disposable users from the publisher.
WITH owner AS (
    UPDATE public.users SET role = 'admin' WHERE id = 'catalogue-owner'
), member AS (
    UPDATE public.users SET invite_code_hash = 'synthetic-playback-invite'
    WHERE id = $2 RETURNING id, email
), invitation AS (
    INSERT INTO public.invite_codes
        (code_hash, used, created_at, issued_by, email, expires_at, redeemed_by, redeemed_at)
    SELECT 'synthetic-playback-invite', true, $3, 'catalogue-owner', email, $4, id, $3 FROM member
    RETURNING id
), session AS (
    INSERT INTO public.sessions (id, user_id, created_at, expires_at, rotation_counter, revoked)
    VALUES ($1, $2, $3, $4, 0, false)
)
SELECT id FROM invitation;
