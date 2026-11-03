-- Regression test for migration 0026 (admin foundation). Run after
-- ALL migrations are applied — twice, since the runner replays every
-- replay-safe migration (see ../run_migration_tests.sh) — so the seed
-- assertions below also prove the replay did not reset or duplicate
-- anything. Everything happens inside one transaction that is rolled
-- back, so it leaves no rows; any broken expectation RAISEs and (with
-- ON_ERROR_STOP) fails the run.

BEGIN;

DO $$
DECLARE
    previous TEXT;
BEGIN
    -- Role default: a freshly inserted user is a member, never an admin.
    INSERT INTO users (id, email, created_at)
    VALUES ('user-admin-1', 'admin-1@example.com', 1700000000),
           ('user-admin-2', 'admin-2@example.com', 1700000000);
    IF (SELECT role FROM users WHERE id = 'user-admin-1') <> 'member' THEN
        RAISE EXCEPTION 'new user did not default to role member';
    END IF;

    -- The CHECK fence: no third role value can be stored.
    BEGIN
        UPDATE users SET role = 'owner' WHERE id = 'user-admin-1';
        RAISE EXCEPTION 'CHECK accepted a role outside member/admin';
    EXCEPTION WHEN check_violation THEN
        NULL;
    END;

    -- Seeded invite-policy settings, exactly once each, still at their
    -- migration defaults after the replay pass.
    IF (SELECT value FROM app_settings WHERE key = 'invite_default_expiry_days') <> '7'
       OR (SELECT value FROM app_settings WHERE key = 'invite_member_min_paid_months') <> '3'
       OR (SELECT value FROM app_settings WHERE key = 'invite_member_per_window') <> '2' THEN
        RAISE EXCEPTION 'app_settings seed missing or altered by replay';
    END IF;
    IF (SELECT count(*) FROM app_settings WHERE updated_by IS NULL) <> 3 THEN
        RAISE EXCEPTION 'expected exactly the three seeded app_settings rows';
    END IF;

    -- The production role-change shape (worker_entry.rs
    -- admin_set_user_role_on): locked read, update, extended audit —
    -- one transaction (this script's own BEGIN stands in for it).
    SELECT role INTO previous FROM users WHERE id = 'user-admin-2' FOR UPDATE;
    UPDATE users SET role = 'admin' WHERE id = 'user-admin-2';
    INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
    VALUES ('user-admin-1', 'user.role_changed', TRUE, '198.51.100.9', 1700000000, 'user-admin-2',
            jsonb_build_object('old', previous, 'new', 'admin'));
    IF previous <> 'member' THEN
        RAISE EXCEPTION 'role-change locked read did not return the previous role';
    END IF;
    IF (SELECT role FROM users WHERE id = 'user-admin-2') <> 'admin' THEN
        RAISE EXCEPTION 'role-change statement did not promote the subject';
    END IF;
    IF (SELECT details ->> 'old' FROM audit_log
        WHERE action = 'user.role_changed' AND subject_user_id = 'user-admin-2') <> 'member'
       OR (SELECT details ->> 'new' FROM audit_log
        WHERE action = 'user.role_changed' AND subject_user_id = 'user-admin-2') <> 'admin'
       OR (SELECT user_id FROM audit_log
        WHERE action = 'user.role_changed' AND subject_user_id = 'user-admin-2') <> 'user-admin-1' THEN
        RAISE EXCEPTION 'extended audit row does not carry actor/subject/old-to-new';
    END IF;

    -- The locked read against a missing subject finds no row; the
    -- production function returns before writing or auditing anything.
    SELECT role INTO previous FROM users WHERE id = 'user-missing' FOR UPDATE;
    IF FOUND THEN
        RAISE EXCEPTION 'locked read found a row for a missing subject';
    END IF;

    -- The production settings-write shape (worker_entry.rs
    -- admin_set_app_setting_on): per-key advisory lock, locked read of
    -- the previous value, upsert, extended audit — one transaction
    -- (this script's own BEGIN stands in for it).
    PERFORM pg_advisory_xact_lock(hashtext('app_settings:' || 'invite_default_expiry_days'));
    SELECT value INTO previous FROM app_settings
    WHERE key = 'invite_default_expiry_days' FOR UPDATE;
    INSERT INTO app_settings (key, value, updated_at, updated_by)
    VALUES ('invite_default_expiry_days', '14', 1700000000, 'user-admin-1')
    ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value,
      updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by;
    INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
    VALUES ('user-admin-1', 'settings.changed', TRUE, '198.51.100.9', 1700000000, NULL,
            jsonb_build_object('key', 'invite_default_expiry_days', 'old', previous, 'new', '14'));
    IF previous <> '7' THEN
        RAISE EXCEPTION 'settings-write locked read did not return the previous value';
    END IF;
    IF (SELECT value FROM app_settings WHERE key = 'invite_default_expiry_days') <> '14'
       OR (SELECT updated_by FROM app_settings WHERE key = 'invite_default_expiry_days') <> 'user-admin-1' THEN
        RAISE EXCEPTION 'settings-write statement did not upsert the row';
    END IF;
    IF (SELECT details ->> 'old' FROM audit_log WHERE action = 'settings.changed') <> '7'
       OR (SELECT details ->> 'new' FROM audit_log WHERE action = 'settings.changed') <> '14'
       OR (SELECT subject_user_id FROM audit_log WHERE action = 'settings.changed') IS NOT NULL THEN
        RAISE EXCEPTION 'settings audit row does not carry the old-to-new values';
    END IF;

    -- Legacy audit rows (no extension columns) still insert cleanly:
    -- pre-0026 writers keep working against the extended table.
    INSERT INTO audit_log (user_id, action, ok, source_ip, at)
    VALUES ('user-admin-1', 'auth.login', TRUE, '198.51.100.9', 1700000000);
    IF (SELECT count(*) FROM audit_log
        WHERE action = 'auth.login' AND subject_user_id IS NULL AND details IS NULL) <> 1 THEN
        RAISE EXCEPTION 'legacy audit insert did not land with NULL extension columns';
    END IF;
END $$;

ROLLBACK;
