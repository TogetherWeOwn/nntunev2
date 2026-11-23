INSERT INTO users (id, email, created_at, role, invite_code_hash) VALUES
    ('private-owner', 'private-owner@example.test', 100, 'admin', NULL),
    ('private-friend', 'private-friend@example.test', 100, 'member', 'private-fixture'),
    ('private-other', 'private-other@example.test', 100, 'admin', 'private-fixture');

INSERT INTO invite_codes (code_hash, used, created_at, issued_by, kind, email, expires_at, redeemed_by, redeemed_at)
VALUES ('private-fixture', TRUE, 100, 'private-owner', 'admin', 'private-friend@example.test', 200, 'private-friend', 199);

DO $$
DECLARE
    listed bigint[] := ARRAY[(SELECT id FROM invite_codes WHERE code_hash = 'private-fixture')];
    access_user text := 'private-friend';
    owner_user text := 'private-owner';
    fixture_window bigint;
BEGIN
    IF NOT pg_temp.private_listening_access(owner_user, owner_user, listed)
       OR NOT pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'owner or redeemed invite was refused';
    END IF;
    IF pg_temp.private_listening_access('private-other', owner_user, listed)
       OR pg_temp.private_listening_access(access_user, 'private-other', listed)
       OR pg_temp.private_listening_access(access_user, owner_user, ARRAY[]::bigint[]) THEN
        RAISE EXCEPTION 'wrong account, wrong owner or removed ID was admitted';
    END IF;
    UPDATE users SET invite_code_hash = NULL WHERE id = access_user;
    IF pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'invite without enrollment provenance was admitted';
    END IF;
    UPDATE users SET invite_code_hash = 'private-fixture' WHERE id = access_user;
    UPDATE invite_codes SET redeemed_at = 200 WHERE id = listed[1];
    IF pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'redemption at the expiry boundary was admitted';
    END IF;
    UPDATE invite_codes SET redeemed_at = 199, revoked_at = 200 WHERE id = listed[1];
    IF pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'revoked invitation was admitted';
    END IF;
    UPDATE invite_codes SET revoked_at = NULL WHERE id = listed[1];
    UPDATE users SET deleting_at = now() WHERE id = access_user;
    IF pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'deleting account was admitted';
    END IF;
    UPDATE users SET deleting_at = NULL WHERE id = access_user;
    UPDATE users SET role = 'member' WHERE id = owner_user;
    IF pg_temp.private_listening_access(owner_user, owner_user, listed)
       OR pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'demoted issuer retained cohort authority';
    END IF;
    UPDATE users SET role = 'admin' WHERE id = owner_user;
    INSERT INTO invite_windows (starts_at, ends_at) VALUES (100, 200) RETURNING id INTO fixture_window;
    UPDATE invite_codes SET kind = 'member', window_id = fixture_window WHERE id = listed[1];
    IF pg_temp.private_listening_access(access_user, owner_user, listed) THEN
        RAISE EXCEPTION 'scheduled member invite was admitted to the private cohort';
    END IF;
END $$;
