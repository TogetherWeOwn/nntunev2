-- Regression test for migration 0030 (admin-issued invites). Run
-- after ALL migrations are applied — twice, since the runner replays
-- every replay-safe migration (see ../run_migration_tests.sh) — so
-- the shape assertions below also prove the replay was a clean
-- no-op. Everything happens inside one transaction that is rolled
-- back, so it leaves no rows; any broken expectation RAISEs and
-- (with ON_ERROR_STOP) fails the run.

BEGIN;

DO $$
DECLARE
    invite_id BIGINT;
    claimed   INT;
BEGIN
    INSERT INTO users (id, email, created_at, role)
    VALUES ('user-invite-admin', 'issuer@example.com', 1700000000, 'admin'),
           ('user-invite-new', 'invitee@example.com', 1700000000, 'member');

    -- Schema shape: surrogate PK on `id`, unique `code_hash`,
    -- required expiry.
    IF (SELECT a.attname
        FROM pg_index i
        JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey)
        WHERE i.indrelid = 'invite_codes'::regclass AND i.indisprimary) <> 'id' THEN
        RAISE EXCEPTION 'invite_codes primary key is not the surrogate id';
    END IF;
    IF (SELECT is_nullable FROM information_schema.columns
        WHERE table_name = 'invite_codes' AND column_name = 'expires_at') <> 'NO' THEN
        RAISE EXCEPTION 'invite_codes.expires_at is not NOT NULL';
    END IF;
    IF (SELECT is_nullable FROM information_schema.columns
        WHERE table_name = 'users' AND column_name = 'invited_by') <> 'YES' THEN
        RAISE EXCEPTION 'users.invited_by missing or NOT NULL';
    END IF;

    -- The kind fence: no third invite kind can be stored.
    BEGIN
        INSERT INTO invite_codes (code_hash, used, created_at, kind, expires_at)
        VALUES ('hash-bad-kind', FALSE, 1700000000, 'partner', 1700604800);
        RAISE EXCEPTION 'CHECK accepted an invite kind outside admin/member';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Duplicate code material is refused by the unique index.
    INSERT INTO invite_codes (code_hash, used, created_at, issued_by, kind, email, expires_at)
    VALUES ('hash-dup', FALSE, 1700000000, 'user-invite-admin', 'admin',
            'invitee@example.com', 1700604800)
    RETURNING id INTO invite_id;
    BEGIN
        INSERT INTO invite_codes (code_hash, used, created_at, expires_at)
        VALUES ('hash-dup', FALSE, 1700000000, 1700604800);
        RAISE EXCEPTION 'duplicate code_hash was accepted';
    EXCEPTION WHEN unique_violation THEN
        NULL;
    END;

    -- The production claim shape (auth.rs SingleUseClaim::Invite,
    -- bound in worker_entry.rs claim_single_use_on): one conditional
    -- UPDATE carrying every spend predicate. It wins exactly once.
    WITH claim AS (
        UPDATE invite_codes SET used = TRUE, redeemed_by = 'user-invite-new',
               redeemed_at = 1700001000
        WHERE code_hash = 'hash-dup' AND used = FALSE AND redeemed_at IS NULL
          AND revoked_at IS NULL AND expires_at > 1700001000
          AND (email IS NULL OR EXISTS (
                SELECT 1 FROM users
                WHERE id = 'user-invite-new' AND lower(email) = lower(invite_codes.email)))
          AND NOT EXISTS (
                SELECT 1 FROM users issuer
                JOIN users redeemer ON lower(redeemer.email) = lower(issuer.email)
                WHERE issuer.id = invite_codes.issued_by AND redeemer.id = 'user-invite-new')
        RETURNING code_hash
    )
    SELECT count(*) INTO claimed FROM claim;
    IF claimed <> 1 THEN
        RAISE EXCEPTION 'first invite claim did not win exactly one row (got %)', claimed;
    END IF;
    WITH claim AS (
        UPDATE invite_codes SET used = TRUE, redeemed_by = 'user-invite-new',
               redeemed_at = 1700001000
        WHERE code_hash = 'hash-dup' AND used = FALSE AND redeemed_at IS NULL
          AND revoked_at IS NULL AND expires_at > 1700001000
        RETURNING code_hash
    )
    SELECT count(*) INTO claimed FROM claim;
    IF claimed <> 0 THEN
        RAISE EXCEPTION 'second invite claim spent an already-spent code';
    END IF;

    -- Account deletion never frees a code: the FK unlinks the
    -- redeemer, the spend stamps stay.
    UPDATE users SET invited_by = 'user-invite-admin',
                     invite_code_hash = 'hash-dup'
    WHERE id = 'user-invite-new';
    DELETE FROM users WHERE id = 'user-invite-new';
    IF (SELECT redeemed_by FROM invite_codes WHERE id = invite_id) IS NOT NULL
       OR (SELECT redeemed_at FROM invite_codes WHERE id = invite_id) IS NULL
       OR NOT (SELECT used FROM invite_codes WHERE id = invite_id) THEN
        RAISE EXCEPTION 'deleting the redeemed account altered the spend state';
    END IF;

    -- Revocation wins against a later claim: a revoked row is
    -- unclaimable even while unexpired and unused.
    INSERT INTO invite_codes (code_hash, used, created_at, issued_by, kind, expires_at,
                              revoked_at, revoked_by)
    VALUES ('hash-revoked', FALSE, 1700000000, 'user-invite-admin', 'admin',
            1700604800, 1700000500, 'user-invite-admin');
    WITH claim AS (
        UPDATE invite_codes SET used = TRUE, redeemed_at = 1700001000
        WHERE code_hash = 'hash-revoked' AND used = FALSE AND redeemed_at IS NULL
          AND revoked_at IS NULL AND expires_at > 1700001000
        RETURNING code_hash
    )
    SELECT count(*) INTO claimed FROM claim;
    IF claimed <> 0 THEN
        RAISE EXCEPTION 'a revoked invite was claimable';
    END IF;
END $$;

ROLLBACK;
