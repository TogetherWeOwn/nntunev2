-- Dormant by default. The protected staging operator enrolls one QA fixture
-- and grants only the views to the dedicated origin login, outside this DDL.
SET search_path = public, pg_catalog;

CREATE SCHEMA IF NOT EXISTS subsonic_measurement;
REVOKE ALL ON SCHEMA subsonic_measurement FROM PUBLIC;

CREATE TABLE IF NOT EXISTS subsonic_measurement.fixture (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    user_id TEXT NOT NULL REFERENCES public.users (id) ON DELETE CASCADE,
    credential_id UUID NOT NULL REFERENCES public.subsonic_credentials (id) ON DELETE CASCADE
);

CREATE OR REPLACE VIEW subsonic_measurement.users WITH (security_barrier = true) AS
SELECT u.id || '' AS id, u.email, u.role, u.deleting_at
FROM public.users u
WHERE u.deleting_at IS NULL AND u.id = (
    SELECT f.user_id FROM subsonic_measurement.fixture f
    JOIN public.subsonic_credentials c ON c.id = f.credential_id AND c.user_id = f.user_id
    WHERE c.auth_mode = 'apikey' AND c.revoked_at IS NULL
);
-- FOR SHARE needs UPDATE on at least one view column. Grant it only on the
-- projected id expression above: PostgreSQL cannot UPDATE a computed column.

CREATE OR REPLACE VIEW subsonic_measurement.subsonic_credentials
WITH (security_barrier = true) AS
SELECT c.id, c.user_id, c.subsonic_username, c.scope, c.auth_mode, c.api_key_hmac,
       c.pepper_version, c.token_secret_encrypted, c.token_envelope_version,
       c.token_payload_format, c.token_kek_version, c.token_dek_wrapped_nonce,
       c.token_dek_wrapped, c.token_payload_nonce, c.revoked_at, c.last_used_at,
       c.attempt_count, c.attempt_window
FROM public.subsonic_credentials c
WHERE c.auth_mode = 'apikey' AND c.revoked_at IS NULL
  AND (c.id, c.user_id) = (
      SELECT f.credential_id, f.user_id FROM subsonic_measurement.fixture f
      JOIN public.users u ON u.id = f.user_id WHERE u.deleting_at IS NULL
  )
WITH CASCADED CHECK OPTION;

REVOKE ALL ON subsonic_measurement.fixture, subsonic_measurement.users,
              subsonic_measurement.subsonic_credentials FROM PUBLIC;
