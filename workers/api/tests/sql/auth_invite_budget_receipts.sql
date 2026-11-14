-- Migration 0041 compatibility and receipt invariants, on the migrated CI database.
BEGIN;

DO $$
DECLARE
    change text;
    bad record;
BEGIN
    INSERT INTO users (id, email, created_at)
    VALUES ('receipt-user', 'receipt-user@example.invalid', 1),
           ('receipt-other', 'receipt-other@example.invalid', 1);

    -- Existing insertion shapes remain valid and default to a null pair.
    INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, purpose)
    VALUES ('receipt-legacy', NULL, 'example.com', 'https://example.com', 10, ''),
           ('receipt-login', 'receipt-user', 'example.com', 'https://example.com', 10, 'login'),
           ('receipt-reauth', 'receipt-user', 'example.com', 'https://example.com', 10, 'reauth'),
           ('receipt-plain', 'receipt-user', 'example.com', 'https://example.com', 10, 'register');
    IF EXISTS (SELECT 1 FROM auth_challenges WHERE id LIKE 'receipt-%'
               AND (invite_budget_code_hash IS NOT NULL OR invite_budget_email_key IS NOT NULL)) THEN
        RAISE EXCEPTION 'legacy insert did not default to null';
    END IF;

    INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, purpose,
                                 invite_budget_code_hash, invite_budget_email_key)
    VALUES ('receipt-bound', 'receipt-user', 'example.com', 'https://example.com', 10,
            'register', 'code', 'email');

    -- Half pairs, unbound registers and non-register receipts must fail.
    FOR bad IN SELECT * FROM (VALUES
        ('half-code', 'receipt-user'::text, 'register', 'code'::text, NULL::text),
        ('half-email', 'receipt-user', 'register', NULL, 'email'),
        ('no-user', NULL, 'register', 'code', 'email'),
        ('login', 'receipt-user', 'login', 'code', 'email'),
        ('reauth', 'receipt-user', 'reauth', 'code', 'email')
    ) AS v(id, user_id, purpose, code, email) LOOP
        BEGIN
            INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, purpose,
                                         invite_budget_code_hash, invite_budget_email_key)
            VALUES ('receipt-bad-' || bad.id, bad.user_id, 'example.com', 'https://example.com',
                    10, bad.purpose, bad.code, bad.email);
            RAISE EXCEPTION 'invalid receipt accepted: %', bad.id;
        EXCEPTION WHEN check_violation THEN NULL;
        END;
    END LOOP;

    -- Every receipt and binding field is frozen, but ordinary consumption is allowed.
    FOREACH change IN ARRAY ARRAY[
        'invite_budget_code_hash = ''other''', 'invite_budget_email_key = ''other''',
        'invite_budget_code_hash = NULL, invite_budget_email_key = NULL',
        'id = ''receipt-moved''', 'user_id = ''receipt-other''', 'user_id = NULL',
        'purpose = ''login''', 'rp_id = ''other.example''',
        'origin = ''https://other.example''', 'expires_at = 11'
    ] LOOP
        BEGIN
            EXECUTE 'UPDATE auth_challenges SET ' || change || ' WHERE id = ''receipt-bound''';
            RAISE EXCEPTION 'receipt mutation accepted: %', change;
        EXCEPTION WHEN restrict_violation THEN NULL;
        END;
    END LOOP;
    BEGIN
        UPDATE auth_challenges SET invite_budget_code_hash = 'code', invite_budget_email_key = 'email'
        WHERE id = 'receipt-plain';
        RAISE EXCEPTION 'retroactive receipt accepted';
    EXCEPTION WHEN restrict_violation THEN NULL;
    END;
    UPDATE auth_challenges SET consumed = TRUE WHERE id IN ('receipt-bound', 'receipt-plain');
    IF (SELECT count(*) FROM auth_challenges WHERE id IN ('receipt-bound', 'receipt-plain') AND consumed) <> 2 THEN
        RAISE EXCEPTION 'consumption failed';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM auth_challenges WHERE id = 'receipt-bound'
                   AND invite_budget_code_hash = 'code' AND invite_budget_email_key = 'email') THEN
        RAISE EXCEPTION 'receipt changed';
    END IF;
END
$$;

ROLLBACK;
