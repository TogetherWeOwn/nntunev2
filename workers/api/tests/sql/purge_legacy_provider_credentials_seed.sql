-- Run against the pre-purge schema inside the caller's test transaction.
INSERT INTO users (id, email, created_at, legacy_vault_established) VALUES
    ('purge-marker', 'purge-marker@example.test', 1, true),
    ('purge-session', 'purge-session@example.test', 1, false),
    ('purge-passkey', 'purge-passkey@example.test', 1, false),
    ('purge-young', 'purge-young@example.test', EXTRACT(EPOCH FROM now())::bigint, false);
INSERT INTO sessions (id, user_id, created_at, expires_at, revoked)
VALUES ('purge-revoked', 'purge-session', 1, 2, true);
INSERT INTO webauthn_credentials (cred_id, user_id, public_key, registered_at)
VALUES ('purge-passkey', 'purge-passkey', '\x00', 1);
INSERT INTO auth_challenges (id, user_id, rp_id, origin, expires_at, purpose)
VALUES ('purge-challenge', 'purge-young', 'example.test', 'https://example.test',
        EXTRACT(EPOCH FROM now())::bigint + 600, 'register');
INSERT INTO provider_vaults (user_id, kek_version, dek_wrapped_nonce, dek_wrapped,
                             payload_nonce, payload, updated_at)
SELECT id, 1, 'fixture', 'fixture', 'fixture', 'fixture', 1 FROM users
WHERE id = 'purge-marker' OR (id = 'purge-young' AND :'purge_case' = 'vault');
INSERT INTO vault_test_leases (user_id, slot, token, expires_at)
VALUES ('purge-marker', 0, 'fixture', 2);
INSERT INTO providers (user_id, label, encrypted_creds, creds_envelope_version, creds_kek_version,
                       creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce,
                       creds_payload_format, created_at, last_verified_at)
SELECT id, 'fixture', '\x00', 2, 1, decode(repeat('00', 12), 'hex'),
       decode(repeat('00', 48), 'hex'), decode(repeat('00', 12), 'hex'), 1, 1, 1
FROM users WHERE id IN ('purge-marker', 'purge-session', 'purge-passkey')
   OR (id = 'purge-young' AND :'purge_case' = 'owner');
INSERT INTO cache_objects (content_hash, byte_size, r2_bucket, r2_key, last_verified_at)
VALUES (repeat('0', 64), 1, 'fixture', 'fixture', 1);
INSERT INTO ingest_jobs (id, user_id, idempotency_key, accepted_at, status)
VALUES ('job_purge_fixture', 'purge-marker', 'ingest:purge-fixture', 1, 'done');
INSERT INTO tracks (id, mbid, release_id, title, medium_no, track_no)
VALUES ('11111111-1111-4111-8111-111111111111', '22222222-2222-4222-8222-222222222222',
        '33333333-3333-4333-8333-333333333333', 'Purge fixture', 1, 1);
INSERT INTO track_media (user_id, track_id, source_job_id, content_hash, format, bitrate, verified_at)
VALUES ('purge-marker', '11111111-1111-4111-8111-111111111111',
        'job_purge_fixture', repeat('0', 64), 'opus', 64, 1);
INSERT INTO user_cache_grants (user_id, content_hash, provider_id, verified_at)
SELECT user_id, repeat('0', 64), id, 1 FROM providers
WHERE user_id = 'purge-marker' AND :'purge_case' = 'grants';
