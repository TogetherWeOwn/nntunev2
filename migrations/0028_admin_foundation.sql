-- Admin foundation: user roles, typed app settings, and an audit log
-- that can carry an actor's subject plus old-to-new change details.
--
-- `users.role` is read fresh from the DB on every admin request (never a
-- session claim). The first admin is promoted by a reviewed operator SQL
-- step (docs/ops.md, "Admin bootstrap"), never self-serve; every later
-- role change goes through the admin API and is audited.
--
-- `audit_log.details` carries ids and old-to-new scalar values only.
-- It must never hold secret or code material (credentials, tokens, invite
-- codes or their hashes): writers reference rows by id, as before.
--
-- Replay-safe: every statement is idempotent, so re-applying this file
-- (or applying it to a database where an operator already added the
-- columns by hand) changes nothing.

ALTER TABLE users ADD COLUMN IF NOT EXISTS role TEXT NOT NULL DEFAULT 'member'
    CHECK (role IN ('member', 'admin'));

CREATE TABLE IF NOT EXISTS app_settings (
    key         TEXT PRIMARY KEY,
    value       TEXT NOT NULL,
    -- Unix epoch seconds of the last write; 0 marks a migration seed
    -- that no admin has overwritten yet.
    updated_at  BIGINT NOT NULL,
    -- The admin who last wrote the row; NULL only for migration seeds.
    updated_by  TEXT REFERENCES users (id)
);

-- Invite policy defaults. Values are decimal strings; the typed
-- accessors in workers/api/src/admin.rs own parsing and bounds.
INSERT INTO app_settings (key, value, updated_at, updated_by) VALUES
    ('invite_default_expiry_days', '7', 0, NULL),
    ('invite_member_min_paid_months', '3', 0, NULL),
    ('invite_member_per_window', '2', 0, NULL)
ON CONFLICT (key) DO NOTHING;

ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS subject_user_id TEXT;
ALTER TABLE audit_log ADD COLUMN IF NOT EXISTS details JSONB;
CREATE INDEX IF NOT EXISTS idx_audit_subject ON audit_log (subject_user_id);
