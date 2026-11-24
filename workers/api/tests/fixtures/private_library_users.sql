UPDATE public.users SET role = 'admin' WHERE id = 'catalogue-owner';
INSERT INTO public.users (id, email, created_at, invite_code_hash) VALUES
    ('library-invitee', 'listener@example.invalid', 1, 'synthetic-library-invite'),
    ('library-unrelated', 'unrelated@example.invalid', 1, 'synthetic-unrelated-invite');
INSERT INTO public.invite_codes
    (id, code_hash, used, created_at, issued_by, email, expires_at, redeemed_by, redeemed_at)
SELECT 900 + row_number() OVER (ORDER BY id), invite_code_hash, true, 1,
       'catalogue-owner', email, 10, id, 2
FROM public.users WHERE id IN ('library-invitee', 'library-unrelated');
