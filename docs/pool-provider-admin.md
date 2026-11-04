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

## Account owner and storage boundary (operations gated)

`account_owner` serializes lazy initialization, reconciliation, reservation and
close/result mutations through one async mutex per account. Construct it once
from the named account Durable Object, never once per request or in the user
meter object. The authenticated registry snapshot must match that object name;
selection hints and request bodies are not registry authority. The owner does
not authenticate snapshots itself. Reconcile a fresh authenticated snapshot
before every admission and release the guard before awaiting socket I/O.
Result recording takes the I/O-completion timestamp and advances the breaker
clock under the same lock; a slow failure must still receive its full cooldown.
Backward wall-clock samples never double-count elapsed time.

`account_storage` stores a single bounded JSON string at
`account-supervisor-v1`. JSON preserves full-width integer revisions and
reservation keys through Worker serialization. A save awaits atomic `put`
**and** `storage.sync()` before acknowledging the journal. A load synchronizes
pending writes first; there is no application read cache. Storage/parse errors
are redacted and never interpreted as a missing journal. Only scheduling
identities, quarantine and outstanding reservations are stored, never endpoint
credentials or resolver response bodies.

Cancellation or uncertainty during initialization leaves that instance fenced;
there is no second lazy initializer over an uncertain write. The supervisor
likewise fences canceled/failed mutation writes. Cleanup uses a separate owner
method that does not need a successful registry read or reconciliation write:
physical close remains possible after poisoning, but uncertain capacity is not
released. Synchronous AUTHINFO checks refuse while the owner lock is held,
rather than block or borrow shared mutable state across an await.

The dedicated `NntpAccount` Durable Object retains the owner in its separate
account namespace. Its authenticated candidate endpoint offers scheduling hints
only, never reservations or credentials. The user meter object remains separate.
The account alarm reconciles registry revocations and drains retained controls,
including after registry failure. Failed/slow refresh cannot suppress cleanup.
Storage promises have bounded waits; an uncertain write still fences admission.
Wakes continue only for live controls or current cancellable reservations, not
forever for recovery-fenced bookkeeping after all physical sockets have drained.

`account_operation` composes one BODY or bounded STAT batch: fresh registry,
durable cleanup wake, durable class reservation, signed exact-identity resolution,
validated ephemeral material, then one literal-peer TLS setup. The proof and
owner's high-water clock are checked at each AUTHINFO command. No credential
retry, hostname re-resolution, warm client, plaintext or user-provider fallback
is allowed. Original resolver buffers and successfully parsed credentials are
zeroized on drop. BODY is bounded to 16 MiB; STAT is bounded to 500 message IDs.
The result names the actual admitted account for eventual ingest attribution.

Known results are synchronously published to reservation-owned completion
latches before awaiting the owner lock. In particular, dropping a failed AUTHINFO
operation while another request owns that lock cannot erase quarantine evidence
or permit another authentication attempt. Admission and cleanup drain those
latches, and explicit close cannot discard them.

Finishing uses one owner mutation lock: apply auth quarantine before any await,
physically close before result-storage waits, persist quarantine and retirement,
and only then release capacity and return output. The whole-session timer also
covers finalization. Expiry returns no output, marks the retained session terminal
and leaves confirmation/retirement to the already-scheduled independent cleanup;
it never waits indefinitely for post-deadline storage. Authenticated registry
absence revokes retained membership, whereas a failed/invalid read does not
impersonate a kill switch. Failed closure/storage never silently frees a slot.
Local proof refusal is not provider ill-health.

`account_dispatch` authenticates an exact `/operation` envelope on the separate
account channel. The envelope names the owning account, a trusted work class and
one BODY ID or bounded STAT batch. Invalid signatures, method/path/browser context,
identity, object shape, unknown/duplicate fields and message IDs refuse before any
registry/storage/resolver/dial work. Each accepted call re-admits through the
retained owner; a signature is not a lease or a replay-proof command. Replays
cannot bypass caps or persistent quarantine. Only this operation path accepts up
to 1 MiB of signed JSON; registry/resolver/candidate requests stay 512-byte bounded.
Success returns a binary BODY or JSON STAT partition, with the actual serving
account in `x-nntune-pool-provider`. Failures are generic and name no account.
All responses are no-store; no remote release/credential endpoint is added.

`account_selection` reads a fresh registry, filters enabled/read/class-eligible
accounts, and requests at most eight concurrent bounded owner hints. Valid hints
rank by tier, then in-flight count, then account ID. Exactly one operation RPC is
attempted; refusal, auth failure, timeout and a lost reply never trigger a second
account or credential attempt. Response identity, kind, BODY size and exact STAT
partition (including duplicate multiplicities) are checked before attribution.
The whole selection call has a <=20-second deadline. Worker client adapters use
only the dedicated `NNTP_ACCOUNT` namespace and never a public URL fallback.

The shared `nntune_core::pool_operation` codec is the single binary BODY/JSON STAT
contract used by stream and ingest. The binding-only per-user object forwards
`/pool-body` and `/pool-stat` envelopes through selection with a **fixed ingest
class**. It validates the routing owner, object shape, duplicate/unknown fields,
message IDs and size before registry/account work; user/job IDs, byte-range hints
and batch indices never enter account operations. This object owns no account
lease or credential material. No user-vault fallback is added.

Ingest calls those new paths, refuses legacy unattributed `200` responses, checks
content type/size and the exact STAT multiset, and retains the actual successful
serving identity. An operation-level missing article is `410`; a route `404` is
unavailable, never missing data. Requests disable redirects, and response I/O
keeps the existing single whole-call deadline and streaming byte ceiling.

Checkpoint `pool_observations` unions confirmed successful account IDs across
batches and BODY fetches, with a 1,000-ID ceiling and explicit truncation flag.
It is **observation evidence, not per-artifact provenance or singular grant
attribution**: retries may span accounts, and scratch adoption or non-atomic
scratch/checkpoint recovery can omit observations. New checkpoints ignore the
legacy queue `provider_id`, and the live consumer clears old hints. The first
account, sole observed account or requested account must never be copied into a
whole-job grant.

`pool_provenance` separately binds complete BODY contributions to decoded bytes
by SHA-256 and size. Each successful, decoded segment stores the actual serving
account with its bytes in the same scratch object write. A lost progress write
can recover that record from object metadata; no successful STAT observation or
queue hint creates one. Assembly checks each present segment's proof against the
bytes actually read, then records the full contributor set against the assembled
file's hash and size. An unknown/legacy segment, changed byte, missing range or
contributor overflow yields **no complete proof**, never a truncated claim.

The contributor ceiling is 32, keeping the complete serialized record and the
existing judgement metadata within R2's 2 KiB custom-metadata budget. JSON
checkpoints default absent records to unknown. Invalid/mismatched metadata is
never promoted to provenance. Spooled staged outputs conservatively have no
complete proof; repaired outputs do not inherit their input proofs. Valid
single-object audio proofs follow their exact source identity into transcode
inputs. These records rely on trusted pipeline scratch/checkpoint writers: the
hash detects stale content, not forged attribution.

This is input provenance evidence, **not a grant or entitlement proof**. Even a
single contributor does not authorize service. Artifact-output mapping, multi-
account grant publication, repair contribution propagation and the independent
publication security gate remain unfinished; no grant writer is enabled by this
increment.

**No operation RPC or product dial is enabled.** The composed operation and ingest
forwarding handlers share the hard false compile-time qualification gate, not an
environment toggle. They refuse before request body or internal key reads. The
ingest consumer is connected to the gated paths; playback misses remain unwired
and unavailable. Native fake-backend tests prove ordering/fencing only, not actual
Worker persistence, alarm delivery, TLS, entropy, clock or physical closure.
Those qualification gates and curated-track staging playback remain required
before enablement.

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
