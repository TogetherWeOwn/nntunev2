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

## Internal account resolution

This channel is not an admin or user API. The stream Worker calls the API
service binding using a dedicated `POOL_RESOLVER_SECRET` (raw string bytes,
at least 32 bytes, API and stream only). Provisioning the key follows the
credential approval procedure; implementation does not provision it. Never
reuse a session, play-grant or metering key. The API alone retains the KEK.
No cookie or Origin is accepted and no OPTIONS/CORS surface is advertised.
Absent/short configuration fails closed with 500; bad authentication is the
ordinary 404, before any registry, DNS or secret read.

The Authorization value is `PoolResolver <unix-seconds>.<lowercase-hex-mac>`.
The MAC is HMAC-SHA256 over the exact concatenation:
`nntune-pool-resolver-v1\nPOST\n<path>\n<unix-seconds>\n<body>` (the `\n`
separators mean newline bytes). The timestamp must be within 30 seconds of
the API clock. This binds the method, endpoint, timestamp and exact JSON
bytes; a request signed for registry listing cannot resolve credentials.
Requests are at most 512 bytes. Service-binding transport is required; the
signature is not a bearer token for browser use. Freshness limits replay,
but does not make a signature single-use. Resolving is read-only and does
not grant an account lease or permission to retry rejected credentials.

- `POST /internal/v1/pool-providers`, body `{}`, returns `{"accounts": [...]}`
  with scheduling snapshots only: `id`, `revision`, `secret_ref`, `can_read`,
  `max_connections`, `stream_reserve`, `ingest_limit`, `tier`, `enabled`.
  Disabled rows remain visible for runtime reconciliation. No endpoint,
  username, password or envelope is listed. More than 1,000 rows refuses.
- `POST /internal/v1/pool-providers/resolve` takes `provider_id`, `revision`,
  `secret_ref` and `class` (`stream` or `ingest`). It joins the current
  registry row and envelope in one uncached database statement, requires an
  exact snapshot match, an enabled read account, and nonzero ingest budget
  for ingest. Unknown fields and invalid identifiers reject with 422.
  Missing, stale, disabled, rotated, unreadable and class-disabled accounts,
  unavailable KEK versions, corrupt ciphertext and forbidden DNS targets
  all return the same generic 503. No alternate KEK or credential retry occurs.

Successful resolve returns `provider_id`, `revision`, `secret_ref`, `host`,
`port`, `tls`, exact public `peer`, `expires_at`, `username` and `password`.
The password is decrypted using only its stored KEK version. All DNS answers
are freshly resolved with the existing bounded, cancellation-safe resolver
and checked by the shared SSRF policy before selecting a peer; IP literals
are checked directly. Material expires five seconds after API request time.
Responses are `Cache-Control: no-store`. Never log or persist these response
bodies, dump them to a console, store them in DO/R2, or send them in queues.

**Runtime safety gate:** this API does not itself dial or enforce a lease.
The account DO must reserve capacity, reconcile its current revision and
quarantine, and consume the result immediately through a transport that dials
only `peer:port` while authenticating TLS against `host`, without re-resolving
the hostname. An expired result or a transport unable to preserve that
pinning must refuse. The existing `WorkerTransport::connect_pinned` checks
TLS/port policy but still dials a hostname; it does **not** satisfy this peer
pinning contract. Until the account runtime and suitable transport land,
NNTP cache misses remain unavailable; this channel alone is not deploy-ready.

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

REA angle: the API reuses the existing admin serialization/role fence, v2
vault field envelope, HMAC request-authentication pattern and bounded DNS/SSRF
guard. No new encryption scheme or failover policy is introduced.
