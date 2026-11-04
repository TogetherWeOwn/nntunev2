# Service account registry administration

The ADR 0014 service registry is managed only through the admin API. It is
not a user provider or credential-vault API. The registry API alone does not
install account scheduling or enable NNTP cache-miss playback.

## Authentication and responses

All endpoints require an authenticated admin cookie session and a fresh
`users.role` lookup. A presented Authorization header is refused even if an
admin cookie accompanies it. Writes additionally require a matching Origin
and recent session reauthentication. The normal admin non-enumeration and
step-up behavior applies; OPTIONS does not advertise these paths.

- `GET /v1/admin/pool-providers` returns `{"providers": [...]}`. Each entry
  contains settings except `username`, plus its revision. Neither password,
  encrypted envelope nor `secret_ref` is returned. Reads refuse a registry
  larger than 1,000 accounts rather than silently hide accounts.
- `POST /v1/admin/pool-providers` creates one account. Body:
  `{"settings": {...}, "expected_revision": 0, "password": "..."}`.
- `PUT /v1/admin/pool-providers/{id}` replaces the settings of one account.
  Body: `{"settings": {...}, "expected_revision": N}`. Include `password`
  only for an explicit credential rotation. The path and settings IDs must
  match. The username must be supplied again; it is never recovered from
  a redacted response.

`settings` contains `id` (lowercase UUID), `label`, `host`, `port`, `tls`,
`username`, `can_read`, `can_post`, `max_connections`, `stream_reserve`,
`ingest_limit`, `backbone`, `tier` and `enabled`. Unknown fields reject.
TLS is mandatory, the port must be 119 or 563, the connection cap is 1–50,
and a read account reserves at least one stream connection. Ingest must fit
within the cap minus the stream reserve. This validation is not a substitute
for DNS/peer validation at dial time.

Create returns 201 and update returns 200 with `{"provider": {...}}` using the
same redacted entry shape as the list. Each successful write increments the
revision once. A duplicate create, missing update target or stale revision
returns 409 and writes nothing. Refresh the list before deliberately applying
a new update; do not blindly retry an old settings replacement.

## Kill switch, credentials and audit

Set `enabled` to false through a revision-checked PUT to disable an account.
Settings-only writes retain the secret identity: disabling/re-enabling or
changing a cap must not clear an authentication quarantine. An explicit
password rotation always creates a new secret identity and a fresh v2 field
envelope using the current API KEK. No credential retry is implied by a
settings write. KEK rewrap is a separate operation and does not rotate identity.

The database transaction takes the shared admin-write lock, rechecks the
actor's live role and deletion fence, locks the target, checks the revision,
then writes the envelope, settings and audit together. Audit failure rolls
back all state. Old ciphertext is removed after rotation only when no registry
account references it. Audit actions are `pool_provider.created`,
`pool_provider.updated`, `pool_provider.disabled`, `pool_provider.caps_changed`
and `pool_provider.credentials_rotated`. Before/after details use redacted
views, never usernames, passwords, envelope bytes or secret references.

Use the approved operator credential-entry procedure. Do not put actual
credentials into shell command lines, logs, source files or task comments.

## Verification

Handler tests use the standard API harness. Production transaction tests run
in the existing PostgreSQL CI job with:

```sh
python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml \
  --features postgres-tests --lib worker_entry::pool_registry_tests
```

`NNTUNE_TEST_DATABASE_URL` must select the disposable `nntune_ci` database on a
loopback host. Tests create isolated schemas and reject nonlocal targets.

This backend/playback work follows AGENTS.md **Clean-room, YouTube and
MetaBrainz guardrails**: no external UI assets or copy, video extraction,
background video playback, or new catalogue/licensing behavior is introduced.

REA angle: the API reuses the existing admin serialization/role fence and v2
vault field envelope. No new protocol, crypto scheme or failover policy is
introduced.
