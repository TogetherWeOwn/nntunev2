# Programmatic ingest session contract (proposal)

This document is inactive preparation, not an endpoint, credential grant,
storage or cryptographic implementation. The operation names and paths below
are proposals, not available APIs. Coordinate upload, authorization and crypto
consumers before activation, including independent Security and QA review;
[ADR 0015](adr/0015-standard-s3-storage-boundary.md) also requires Applied
Crypto approval of the encryption design. Existing administrator routes do not
grant these permissions.

## Operations and authorization

- `POST /v1/ingest/sessions`: propose a FLAC session with bounded metadata,
  declared length, a claimed plaintext SHA-256 and an idempotency-key header.
- `GET /v1/ingest/sessions/{session_id}`: propose returning the owned session's
  ingest job status. Agree the exposed status vocabulary with consumers.
- `POST /v1/ingest/sessions/{session_id}/abort`: propose an owned session abort.
  Agree terminal-state and concurrent completion behavior before activation.

Each operation must recheck the authenticated grant, expiry, revocation,
environment, library, ownership and relevant create, status, abort or reuse
action; never trust a caller-supplied owner. The inactive authorization helper
in `workers/api/src/ingest_session_auth.rs` is not a route or a credential
issuer. The internal Worker-to-Worker ingest grant is distinct from the native
bearer scope. Neither the existing NZB routes nor their size limits specify this
new API. Bound the request body at the transport boundary before parsing, as
well as bounding metadata, declared size and deployment resource use. Exact
limits and wire schemas require consumer agreement before activation.

## Idempotency and upload integrity

After authorization, scope each idempotency key by environment, library and
authenticated principal. Atomically reserve that tuple with a versioned request
fingerprint and intended session identity. A same-key, same-request retry
returns that identity; a changed request conflicts. Define the fingerprint over
a domain/version, declared length, claimed digest and unambiguous metadata,
including optional-field presence. Agree retention, concurrency and post-expiry
behavior before activation. This proposed reservation is distinct from existing
admin content-hash deduplication and is not implemented by this document.

Fingerprints, keys, metadata and digests are sensitive: do not log them or put
them in errors. Return redacted typed errors and approved opaque audit
identifiers. Synthetic fixtures must not contain real media or credentials.
A caller's digest, declared length and metadata do **not** prove received byte
equality or grant no-upload reuse. Verify actual plaintext digest and FLAC,
complete authenticated upload and identity checks before publication. Reuse
requires explicit authorization for a fully validated object in the permitted
namespace; caller claims alone never establish ownership or eligibility.

The off-route encrypted upload caller can admit short-lived reuse of the
exact committed object from an owned session. It resolves immutable version
pins from the final checkpoint and verified winner, then checks the current
reuse grant under the session and winner locks. The protection lasts at most
30 seconds, bounded by credential expiry, session expiry and winner retention.
The returned pins carry no provider access. A future destination caller must
authorize and bind its reference through guarded reference admission.

The off-route upload attempt checks this scoped result before opening an upload.
A reused result carries the durable receipt pins and protection deadline.
Compare those pins with authenticated readback when consuming receipt evidence.
A refusal can fall through to the normal upload lease, which only opens a live,
unstarted reservation. An unconfirmed result stops the attempt for reconciliation.

## Large uploads and activation requirements

Follow ADR 0015's authenticated, resumable multipart design. Direct object
storage transfers carry ciphertext only; never expose provider credentials or
buffer whole files. The transfer protocol, client requirements and any browser
CORS policy need consumer agreement; this proposal does not mandate a particular
transport or claim an existing multipart pipeline. Before activation, consumers
must agree which declared lengths describe plaintext and ciphertext, the
per-piece and total ciphertext lengths and transfer caps, and how framing
affects those lengths; ciphertext counts must not be compared directly with a
plaintext declaration. At the receiving or storage boundary, use trusted
observed byte counts to enforce each piece and the total ciphertext transfer;
terminate transfers as soon as an agreed per-piece or total ciphertext length
or transfer cap is exceeded, and reject completion if a piece or total is
truncated or differs from its agreed length. If a
plaintext length is declared, verify the observed plaintext length against it
before publication. Bind each piece/checkpoint
to environment, library, session, generation, offset, request fingerprint,
lease revision and expiry. Accepted piece replay must be byte-identical; changed
replay conflicts. If exact ciphertext replay or a trustworthy checkpoint is
unavailable, fence, abort and reconcile before a fresh lifecycle; never reseal
changed plaintext under an old lifecycle.

The unmounted multipart handoff commits an object claim under the initial
write lease before sending CreateMultipartUpload, then records the exact
provider upload ID under the same fence. Parts require the committed target.
Recover it with the current scoped checkpoint and lease before resuming, and
verify the trusted sealer state and exact ciphertext independently.

A claim without an ID may still be in flight. A racing caller can re-read it
but cannot send another initiation or treat the pending state as checkpoint
loss. After a confirmed interruption with a lost ID, fence the matching
snapshot through the existing checkpoint-loss transition or let it expire.
Reconcile the original object under the resulting cleanup intent. If S3
may have created an upload whose ID was never recorded, cleanup must cover
that exact object key, including late arrivals. Staging activation needs a
verified incomplete-upload lifecycle policy on its configured bucket as a
backstop. A new attempt uses fresh object and sealing identities.

## Staging database handoff

The [app migration](../migrations/0062_ingest_sessions.sql)
creates reservations and durable cleanup intents. It depends on the app
namespace, object-intent and digest-inventory migrations (0042, 0043 and 0055)
and the existing users table. The new tables start with owner-only ACLs even
when the migrator has default grants for the ordinary app roles. Hosted CI
rehearses the registered bytes on a fresh app install and a populated upgrade.

Use the
[protected staging apply procedure](ops.md#staging-migrations)
with independent approval for the exact DDL and a successful staging ledger
receipt before merge. For a failed or uncertain apply, reconcile both the
receipt and table state before retrying. On a confirmed apply, leave the
additive schema and receipt in place when rolling back a caller.

Provision the isolated staging writer separately, without inherited app
roles, using the [scoped grant fixture](../workers/api/tests/fixtures/ingest_session_writer_grants.sql)
as the access matrix for the upload and cleanup queries. The trusted caller
must enforce environment, library and principal scope in every operation.
The upload role has no session deletion or publication write grant. Before
staging writes, agree how principal erasure and namespace retirement handle
retained reservations. Their foreign keys forbid dropping referenced rows.
Keep routes and cleanup scheduling off until isolated caller wiring, the
provider and bucket checks above, and review of the cleanup behavior are
complete. Publication authority has its own rollout.

The opt-in API caller opens a signed upload from a scoped reservation and
pre-enrolled intent, commits after authenticated readback, and replays durable
pins. The [completion grants fixture](../workers/api/tests/fixtures/ingest_session_caller_grants.sql)
covers inventory and outcome writes. Replay and the immediate post-commit read
lock the scoped session before loading current credential and upload grant state
through the trusted authorization reader. Use current, non-locking reads with
separate access and keep credential and grant stores outside the upload writer's
grants. Hosted fixtures cover waiting replay, revocation during the lock wait and rollback
when the session expires after winner staging. Connect the sealed data producer
and provision the live authorization reader and scoped staging grants separately.
Wire scoped cleanup after bucket qualification.

Proposed lifecycle bounds are at most 24 hours absolute and 60 minutes idle
since accepted progress; status and retries do not extend absolute expiry.
Consumers must agree limits, tombstones, lease fencing, cleanup, and race and
terminal behavior before activation. Validate multipart completion and object
integrity before publication; a successful part response or a completion
response containing an error is not proof of completion. Apply ADR 0015's
client-side envelope encryption and app-side plaintext deduplication decisions;
cryptographic format, chunk size and multipart validation require the prescribed
Security and Applied Crypto review. This document does not implement any of
these obligations.
