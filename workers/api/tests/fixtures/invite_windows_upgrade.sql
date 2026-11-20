-- Run at the upgrade boundary on the disposable CI database. Roll back both
-- the fixture and the migration so the main runner still tests a clean apply.
BEGIN;

INSERT INTO users (id, email, created_at, role)
VALUES ('window-admin', 'window-admin@example.com', 50, 'admin'),
       ('window-member', 'window-member@example.com', 50, 'member');
INSERT INTO invite_codes (code_hash, used, created_at, expires_at)
VALUES ('window-fixture-admin-default', FALSE, 50, 1000);
INSERT INTO invite_codes (code_hash, used, created_at, expires_at, issued_by, revoked_at, revoked_by)
VALUES ('window-fixture-admin-revoked', FALSE, 50, 1000, 'window-admin', 60, 'window-admin');
CREATE TEMP TABLE admin_invite_snapshot AS
SELECT id, to_jsonb(c) AS row_data FROM invite_codes c WHERE kind = 'admin';

\i :migration

DO $$
DECLARE
    first_window BIGINT;
    next_window BIGINT;
    cancelled_window BIGINT;
BEGIN
    INSERT INTO invite_windows (starts_at, ends_at, created_by, note)
    VALUES (100, 200, 'window-admin', 'First window') RETURNING id INTO first_window;
    INSERT INTO invite_windows (starts_at, ends_at, created_by)
    VALUES (200, 300, 'window-admin') RETURNING id INTO next_window;
    INSERT INTO invite_windows (starts_at, ends_at, created_by)
    VALUES (400, 500, 'window-admin') RETURNING id INTO cancelled_window;

    BEGIN
        INSERT INTO invite_windows (starts_at, ends_at) VALUES (125, 175);
        RAISE EXCEPTION 'overlapping insert accepted';
    EXCEPTION WHEN exclusion_violation THEN NULL;
    END;
    BEGIN
        UPDATE invite_windows SET ends_at = 201 WHERE id = first_window;
        RAISE EXCEPTION 'overlapping extension accepted';
    EXCEPTION WHEN exclusion_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO invite_windows (starts_at, ends_at) VALUES (550, 510);
        RAISE EXCEPTION 'backwards window accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;

    BEGIN
        INSERT INTO invite_codes (code_hash, used, created_at, expires_at, kind, window_id)
        VALUES ('window-fixture-bad-admin', FALSE, 105, 300, 'admin', first_window);
        RAISE EXCEPTION 'window accepted on an admin invite';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO invite_codes (code_hash, used, created_at, expires_at, kind)
        VALUES ('window-fixture-bad-member', FALSE, 105, 300, 'member');
        RAISE EXCEPTION 'windowless member invite accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO invite_codes (code_hash, used, created_at, expires_at, kind, window_id)
        VALUES ('window-fixture-orphan', FALSE, 105, 300, 'member', -1);
        RAISE EXCEPTION 'unknown window accepted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;

    INSERT INTO invite_codes (code_hash, used, created_at, expires_at, kind, window_id, issued_by, revoked_at)
    VALUES ('window-fixture-expired', FALSE, 105, 130, 'member', first_window, 'window-member', NULL),
           ('window-fixture-revoked', FALSE, 106, 300, 'member', first_window, 'window-member', 120),
           ('window-fixture-other-window', FALSE, 200, 300, 'member', next_window, 'window-member', NULL);
    UPDATE invite_windows SET ends_at = 150 WHERE id = first_window;
    UPDATE invite_windows SET ends_at = starts_at WHERE id IN (next_window, cancelled_window);
    -- Rebook the shortened tail, the window closed at its start (even with
    -- issued rows), and the cancelled future window.
    INSERT INTO invite_windows (starts_at, ends_at, created_by)
    VALUES (150, 200, 'window-admin'), (200, 300, 'window-admin'), (400, 500, 'window-admin');

    IF (SELECT count(*) FROM invite_codes
        WHERE window_id = first_window AND issued_by = 'window-member') <> 2 THEN
        RAISE EXCEPTION 'shortening refunded expired or revoked issue rows';
    END IF;
    BEGIN
        DELETE FROM invite_windows WHERE id = next_window;
        RAISE EXCEPTION 'window attribution could be deleted';
    EXCEPTION WHEN foreign_key_violation THEN NULL;
    END;
END $$;

\i :migration

DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM admin_invite_snapshot s
        FULL JOIN (SELECT * FROM invite_codes WHERE kind = 'admin') c USING (id)
        WHERE s.row_data IS DISTINCT FROM CASE WHEN c.id IS NOT NULL THEN to_jsonb(c) END
    ) THEN
        RAISE EXCEPTION 'upgrade or replay changed an admin invite';
    END IF;

    -- The count can pass via a sequential scan; check the index separately.
    IF NOT EXISTS (
        SELECT 1 FROM pg_catalog.pg_index i
        WHERE i.indrelid = 'public.invite_codes'::regclass
          AND i.indexrelid = to_regclass('public.idx_invite_codes_window_issuer')
          AND i.indisvalid
          AND i.indnkeyatts = 2
          AND pg_get_indexdef(i.indexrelid, 1, false) = 'window_id'
          AND pg_get_indexdef(i.indexrelid, 2, false) = 'issued_by'
          AND pg_get_expr(i.indpred, i.indrelid) = '(window_id IS NOT NULL)'
    ) THEN
        RAISE EXCEPTION 'missing or invalid invite quota index covering all window-linked rows';
    END IF;
END $$;

ROLLBACK;
