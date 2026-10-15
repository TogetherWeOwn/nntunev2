# ADR 0002: Data Model + ERD (PlanetScale Postgres via Hyperdrive)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — preserve merged auth; pin catalog identity, atomic
  stream accounting, relational price mappings and bounded renewal settlement.
- **Baseline inspected:** main `29140710bb5d8e0d57f1701ca1e6c3e854e9104f`.
  Auth, ingest, core, scaffold and MB work are merged, not in-flight assumptions.

## Context and compatibility boundary

All relational data uses PlanetScale Postgres through Hyperdrive. The app uses
`nntunev2_staging` in staging; the mirror uses its separate cluster. `docs/ops.md`
is authoritative for provisioned targets. A production binding does not mean a
clean application DB or production writes/migrations are approved.

**Preserve the implemented auth schema.** `migrations/0002_auth_vault.sql` and
[ADR 0008](0008-auth-vault-sessions.md) remain authoritative. The earlier draft's
replacement of auth TEXT ids/BIGINT timestamps with UUID/timestamptz is withdrawn.
New `user_id` FKs are **TEXT**, matching `users.id`; do not cast or regenerate
existing identities. New app-domain ids may be UUID and new app timestamps are
`timestamptz`. Convert legacy epoch **seconds** at explicit DTO boundaries
(`to_timestamp(value)` / epoch extraction), not via implicit SQL casts.

### Existing auth tables — retained, not recreated

```text
users                 id TEXT PK, email TEXT UNIQUE, created_at BIGINT,
                      invite_code_hash TEXT NULL
webauthn_credentials   cred_id TEXT PK, user_id TEXT FK, public_key BYTEA,
                      sign_count BIGINT DEFAULT 0, registered_at BIGINT
auth_challenges       id TEXT PK, user_id TEXT NULL (no FK in current schema),
                      rp_id TEXT, origin TEXT, expires_at BIGINT,
                      consumed BOOLEAN DEFAULT FALSE
magic_link_tokens     token_hash TEXT PK, user_id TEXT FK, email TEXT,
                      issued_at BIGINT, expires_at BIGINT, used BOOLEAN DEFAULT FALSE
sessions              id TEXT PK, user_id TEXT FK, created_at BIGINT,
                      expires_at BIGINT, rotation_counter INTEGER DEFAULT 0,
                      revoked BOOLEAN DEFAULT FALSE
provider_vaults       user_id TEXT PK/FK, kek_version INTEGER,
                      dek_wrapped_nonce TEXT, dek_wrapped TEXT,
                      payload_nonce TEXT, payload TEXT,
                      indexer_configured BOOLEAN DEFAULT FALSE, updated_at BIGINT
invite_codes          code_hash TEXT PK, used BOOLEAN DEFAULT FALSE, created_at BIGINT
audit_log             id BIGSERIAL PK, user_id TEXT, action TEXT,
                      ok BOOLEAN, source_ip TEXT, at BIGINT
```

Nullability/FKs not repeated above are as defined by that migration. In
particular, `sessions.rotation_counter` and `revoked` are not replaced by a
`revoked_at` invention. Refresh must conditionally advance the stored counter,
commit exactly one updated row, then send the cookie (ADR 0008). Auth challenges
and audit rows are not silently given new cascade constraints by this diagram.

### Additive target tables — implementation work remains

Unless stated otherwise, new ids are UUID, timestamps are `timestamptz`, and
`user_id` is TEXT FK → users. Encryption field groups are expanded in rule 1.

```text
subsonic_credentials  id PK, user_id FK, label,
                      subsonic_username TEXT UNIQUE (generated non-PII),
                      api_key_hmac TEXT NULL, pepper_version INTEGER NULL,
                      UNIQUE (pepper_version, api_key_hmac),
                      token_secret_encrypted BYTEA NULL + token_* envelope fields,
                      auth_mode TEXT ('apikey'|'token'|'both', default 'apikey'),
                      scope TEXT DEFAULT 'subsonic', created_at, last_used_at, revoked_at
plans                 id TEXT PK ('free','plus','pro-5x','pro-20x'), price_cents,
                      stream_hours_month, max_concurrent_streams, max_library_tracks,
                      ingest_jobs_day, cache_priority,
                      cache_bytes_budget BIGINT, cache_retention_days INTEGER
plan_prices           stripe_price_id TEXT PK, plan_id FK → plans NOT NULL,
                      currency TEXT NOT NULL, price_cents INTEGER NOT NULL,
                      billing_interval TEXT NOT NULL, livemode BOOLEAN NOT NULL,
                      selectable BOOLEAN NOT NULL DEFAULT FALSE,
                      partial UNIQUE (plan_id) WHERE selectable
billing_accounts      user_id TEXT PK/FK, stripe_customer_id TEXT UNIQUE NULL,
                      trial_started_at NULL (write-once after confirmed trial creation)
billing_operations    id PK, user_id FK, kind TEXT, state TEXT,
                      request_key TEXT UNIQUE, request_payload JSONB,
                      stripe_subscription_id TEXT NULL, stripe_invoice_id TEXT NULL,
                      pending_update_expires_at NULL, created_at, updated_at,
                      partial UNIQUE (user_id) WHERE state IN
                      ('pending','submitted','needs_reconcile')
billing_webhook_events stripe_event_id TEXT PK, received_at, processed_at NULL
subscriptions         id PK, user_id FK,
                      stripe_subscription_id TEXT UNIQUE NOT NULL,
                      stripe_customer_id TEXT NOT NULL,
                      stripe_subscription_item_id TEXT NOT NULL,
                      stripe_price_id TEXT NOT NULL (observed provider state),
                      plan_id FK → plans (last confirmed entitlement plan),
                      status TEXT (Stripe status, rule 4), current_period_end,
                      cancel_at_period_end BOOLEAN DEFAULT FALSE,
                      trial_end NULL, paid_through_at NULL, paid_invoice_id TEXT NULL,
                      grace_until NULL,
                      partial UNIQUE (user_id) WHERE status IN
                      ('incomplete','trialing','active','past_due','unpaid','paused')
usage_ledger          id BIGSERIAL PK, user_id FK, period DATE (UTC day),
                      stream_seconds BIGINT NOT NULL DEFAULT 0 CHECK (stream_seconds >= 0),
                      ingest_jobs BIGINT NOT NULL DEFAULT 0 CHECK (ingest_jobs >= 0),
                      UNIQUE (user_id, period) -- flows only, no cache gauge
ingest_jobs           id PK, user_id FK, idempotency_key TEXT, accepted_at,
                      status TEXT ('queued'|'running'|'done'|'failed'),
                      UNIQUE (user_id, idempotency_key)
providers             id PK, user_id FK, label,
                      encrypted_creds BYTEA + creds_* envelope fields,
                      created_at, last_verified_at, UNIQUE (user_id, id)
indexers              id PK, user_id TEXT FK NULL (NULL means global),
                      name, base_url, api_key_encrypted BYTEA NULL
                      + api_key_* envelope fields, enabled BOOLEAN
libraries             id PK, user_id FK, name, created_at
artists               id PK, mbid UUID NULL (MB artist), local_key TEXT UNIQUE NULL,
                      name, partial UNIQUE (mbid) WHERE mbid IS NOT NULL
releases              id PK, mbid UUID NULL (MB release), local_key TEXT UNIQUE NULL,
                      release_mbid_hint UUID NULL (non-canonical reconciliation hint),
                      artist_id FK, title, release_type, year,
                      partial UNIQUE (mbid) WHERE mbid IS NOT NULL
tracks                id PK, mbid UUID NULL INDEXED (MB recording, NOT unique),
                      release_id FK NOT NULL, title, medium_no INTEGER NOT NULL,
                      track_no INTEGER NOT NULL, duration_ms,
                      UNIQUE (release_id, medium_no, track_no)
library_tracks        library_id FK, track_id FK, added_at, PK (library_id, track_id)
track_sources         user_id FK, track_id FK, source_job_id FK → ingest_jobs,
                      indexer_id FK NULL, nzb_ref_encrypted BYTEA
                      + nzb_ref_* envelope fields, PK (user_id, track_id)
track_media           user_id FK, track_id FK, source_job_id FK → ingest_jobs,
                      content_hash FK → cache_objects, format TEXT, bitrate INTEGER,
                      verified_at, last_served_at,
                      PK (user_id, track_id, source_job_id, content_hash)
user_cache_grants     user_id FK, content_hash FK → cache_objects,
                      provider_id UUID NOT NULL,
                      FK (user_id, provider_id) → providers(user_id, id) ON DELETE RESTRICT,
                      verified_at, last_served_at, PK (user_id, content_hash)
cache_objects         content_hash TEXT PK (sha256 hex), byte_size BIGINT, r2_key TEXT,
                      ref_count BIGINT (advisory), created_at, last_verified_at
playlists             id PK, user_id FK, name, created_at
playlist_tracks       id PK, playlist_id FK, track_id FK, position INTEGER,
                      UNIQUE (playlist_id, position) DEFERRABLE INITIALLY DEFERRED,
                      INDEX (playlist_id, track_id)
play_event_outbox     event_id UUID PK, user_id FK, track_id FK,
                      played_at timestamptz NOT NULL, stream_seconds BIGINT NOT NULL,
                      cached_hit BOOLEAN, delivered_at NULL,
                      CHECK (stream_seconds >= 0)
play_events           event_id UUID NOT NULL, user_id FK, track_id FK,
                      played_at timestamptz NOT NULL, stream_seconds BIGINT NOT NULL,
                      cached_hit BOOLEAN, PK (event_id, played_at),
                      CHECK (stream_seconds >= 0),
                      PARTITION BY RANGE (played_at), monthly
```

The ingest job's payload is retained by the dispatch/queue path; a durable
operation/outbox must make DB acceptance and enqueue retryable without counting
a job twice. An accepted job is not evidence that normalized library tables
or media already exist.

### ERD (logical relationships; actual FKs as above)

```text
users ──< webauthn_credentials / magic_link_tokens / sessions
users ──1 provider_vaults (existing, retained)
users ──< auth_challenges / audit_log (logical ownership)
invite_codes ──< users (invite_code_hash, logical reference)
users ──1 billing_accounts ──< billing_operations
users ──< subscriptions >── plans ──< plan_prices
users ──< subsonic_credentials / providers / indexers
users ──< usage_ledger / ingest_jobs / play_event_outbox / play_events
users ──< libraries ──< library_tracks >── tracks >── releases >── artists
users ──< track_sources >── tracks
users ──< track_media >── tracks
track_media >── cache_objects <── user_cache_grants >── users
providers ──< user_cache_grants
users ──< playlists ──< playlist_tracks >── tracks
```

## Key rules

### 1. Encrypted payloads have a versioned, authenticated envelope

Existing `provider_vaults` is **legacy envelope v1** with exact AAD already used
by `workers/api/src/vault.rs`: wrap `v1|dek|{user_id}|{kek_version}` and payload
`v1|vault|{user_id}`. Preserve its TEXT/base64url fields and decoding. A later
migration must read that format explicitly; merely changing AAD breaks decrypt.

Each **new** encrypted field has the following six companion columns (replace
`prefix` with `creds`, `api_key`, `token`, or `nzb_ref` from the target table):

```text
prefix_envelope_version SMALLINT  -- 2 for this new field format
prefix_kek_version INTEGER        -- exact wrapping-key version, not "latest"
prefix_dek_wrapped_nonce BYTEA    -- 12 bytes
prefix_dek_wrapped BYTEA          -- AES-256-GCM wrapped 32-byte random DEK + tag
prefix_payload_nonce BYTEA        -- 12 bytes
prefix_payload_format SMALLINT   -- plaintext schema/version
```

The named `*_encrypted` BYTEA column stores payload ciphertext plus its GCM tag.
Use a fresh per-field 32-byte DEK and fresh 12-byte nonces. For mandatory fields
all companions are NOT NULL; for optional fields a CHECK requires **all present
or all NULL**. `token_*` is present exactly when `auth_mode` includes `token`.

AAD uses one canonical length-prefixed encoding (not ambiguous delimiter joins):

- Payload: `(domain="nntune-envelope-v2", purpose="payload", table, field,
  owner_scope, row_key, envelope_version, payload_format)`.
- DEK wrap: the same tuple with `purpose="dek"`, plus `kek_version`.

`owner_scope` is the exact user id, or an explicit tagged global scope for a
NULL-owner indexer; `row_key` is the row id, or `(user_id, track_id)` for a source.
Build expected AAD from the row being read, not a stored unauthenticated claim.
Cross-user, cross-row and cross-field substitution must fail. Payload AAD
intentionally excludes `kek_version`: authorized rewrap changes **only**
`prefix_kek_version`, `prefix_dek_wrapped` and `prefix_dek_wrapped_nonce` atomically.
`prefix_envelope_version`, `prefix_payload_format`, payload nonce and ciphertext
remain unchanged. A format migration is not a KEK rewrap.

Decrypt selects the exact KEK version from the secret keyring; unknown versions
fail closed. Rewrap uses compare-and-swap on the old row's KEK version, wrapped
DEK and wrap nonce so it cannot overwrite a concurrent credential update. Any
payload replacement creates a fresh DEK/envelope, including new wrap values.
Keep the previous KEK until
all live rows and retained backups have a verified decrypt/restore path. Any
actual rotation/retirement needs its existing security authorization and no-downtime
procedure; this ADR performs or approves no credential mutation.

### 2. Subsonic credential lookup and pepper lifecycle

Default `auth_mode='apikey'` stores HMAC-SHA256(apiKey) under the current
`SUBSONIC_INDEX_PEPPER` version, with `pepper_version`. That Worker secret is a
**versioned keyring**, not a single replace-in-place pepper: its structured value
contains `current_version`, optional `previous_version`, and the corresponding
version→key map. At most those two versions are loaded; key material stays in
Worker secrets, never ordinary config or SQL. Validate both referenced entries
before deploying a transition. Generate 256-bit CSPRNG keys server-side (128 bits
is the minimum entropy floor), show once, never store plaintext. Requests carry
`apiKey` **without `u`**. Bare SHA-256 is not the target.

During an authorized pepper transition, try at most current and previous
version/digest pairs against the composite unique index. On an old-version match,
compare-and-swap its HMAC/version to current using the presented key. **A stored
HMAC cannot be migrated without plaintext.** Retire an old pepper only when no
unrevoked credentials still use it; dormant clients must authenticate/migrate
or explicitly reissue their credentials through an authenticated flow first.
No time-based silent retirement that locks them out, and no plaintext recovery.
Do not start another transition while an older supported version remains.

The opt-in `t`/`s` form needs a reversible secret. Look up the generated non-PII
`subsonic_username` (never email), unwrap exactly that row's versioned token
envelope, and verify `md5(secret + salt)`. CHECK constraints require HMAC/version
for `apikey`/`both`, token envelope for `token`/`both`, and forbid unintended
extra credential material. `both` uses independently generated API-key and token
secrets, never a reversible copy of the HMAC-indexed API key. Scope, revocation
and rate limits apply before handler access. These credentials do not authenticate
native REST (ADR 0006).

### 3. Library, source and byte ownership have distinct writers

- **API accept transaction:** `ingest_jobs` plus its daily count, idempotent on
  `(user_id, idempotency_key)`; enqueue via a retryable outbox/dispatch contract.
- **Ingest `write` stage, after `match`:** normalized tracks, membership and
  encrypted `track_sources` in one transaction. Preserve an ordered per-track
  segment manifest or recoverable NZB with file/track selection. A short-lived
  URL or the existing 30-day `INGEST_PAYLOADS` KV entry is not a lifetime source.
  Unmatched tracks have local ids and NULL MBIDs. This stage writes no media hash.
- **Stream worker, after full own-provider fetch and hash verification:**
  `cache_objects`, per-user `track_media`, and `user_cache_grants`. Initialize
  both `last_served_at` columns and refresh the grant/served mapping on serve.

Eviction removes grants, then orphaned mappings, then unreferenced objects (ADR
0003); it never removes sources. Final removal from all of a user's libraries
removes that user's source/mappings, not other users' copies. A live grant is
required even when an object or track id is known. `cache_bytes` is a live
SUM of object sizes across that user's unique grants, not a daily/monthly flow.

### 4. Subscription slots are not the same as entitlement

ADR 0005 defines the full creation/conversion/reconciliation contract. There is
**no provisional NULL-id subscription**. API-response and webhook reconciliation
use the same real Stripe id; paid conversion updates that same subscription/item.
`billing_operations` prevents concurrent creates and preserves retry identity.
`billing_webhook_events` makes duplicate delivery processing idempotent.

Store all eight Stripe statuses: `incomplete`, `incomplete_expired`, `trialing`,
`active`, `past_due`, `canceled`, `unpaid`, `paused`. The six nonterminal statuses
reserve the partial-unique user slot; only confirmed `canceled` and
`incomplete_expired` release it. There is no local `expired` shortcut that frees
a slot while a remote subscription can still bill.

Entitlement is derived from the single row:

- `trialing AND trial_end > now()` → free caps.
- `active AND plan_id != 'free'` → the confirmed paid plan while
  `paid_through_at > now()`, **or** while `cancel_at_period_end=false` and
  `now() < paid_through_at + interval '3 hours'`. The latter is the fixed
  `RENEWAL_SETTLEMENT_ALLOWANCE`, covering Stripe's normal draft/charge window
  and webhook delay, not evidence of payment. It requires a non-NULL previously
  paid end, never advances it and cannot slide on a retry. An `active` free
  subscription has no entitlement. Scheduled cancellation gets no allowance.
- `past_due` with a previous paid plan → that plan while `paid_through_at > now()`
  (including a failed mid-period upgrade), or with `grace_until > now()` during
  the fixed seven-day **renewal-only** grace. No grace for first payment.
- Anything else → no entitlement. `users` has no plan column.

The exact trial setting is
`trial_settings.end_behavior.missing_payment_method = cancel`; additionally
schedule end-of-trial cancellation regardless of card presence (ADR 0005).
The API's hourly `billing-reconcile` job handles elapsed grace without a webhook;
the new-purchase path runs the same reconciliation inline. Both confirm remote
termination before replacement. Timestamp predicates cut access immediately even
if the job is delayed. `cancel_at_period_end` on a still-active paid subscription
is not terminal cancellation.

### 5. Flows, gauges and partitions

`play_events` is append-only and monthly-partitioned. Partition maintenance
creates future partitions and alerts if one is missing; no silent dropped writes.
Retention/drop policy requires its own approved operation, not this ADR.
The stream writer generates each `event_id` and its exact UTC `played_at` **once
before dispatch**, committing the immutable event to `play_event_outbox`.
An outbox dispatcher retries undelivered rows; it sets `delivered_at` only after
the event/ledger transaction commits. Queue/Worker retries reuse both and the
same payload; never regenerate
`played_at=now()` on delivery. Each event carries an **incremental**, non-overlapping
number of seconds, not a cumulative playback total. Split events at UTC midnight
so each belongs to one daily/monthly cap period. The composite PK includes the
partition key as PostgreSQL requires; `UNIQUE(event_id)` alone is invalid here.

**No asynchronous rollup or independent checkpoint.** In one DB transaction,
`INSERT INTO play_events ... ON CONFLICT (event_id, played_at) DO NOTHING RETURNING
user_id, played_at, stream_seconds` feeds a grouped daily ledger upsert. Increment
`usage_ledger.stream_seconds` only by those **inserted/returned** rows, with
`period=(played_at AT TIME ZONE 'UTC')::date`. Both counters are `NOT NULL DEFAULT 0`;
stream and ingest writers insert or increment only their own counter, preserving
zero for the other. Use additive `ON CONFLICT (user_id, period) DO UPDATE` so
concurrent stream/ingest writers cannot lose one another's increments. Commit
before acknowledging delivery. A crash before commit
rolls back both; a crash after commit redelivers as a zero-increment no-op. A
missing partition or ledger error aborts both and leaves the event retryable.
The committed event row is the durable dedupe receipt; no undefined checkpoint
table is needed. Producers must reject attempts to reuse an identity for a changed
payload. Retention must preserve receipts through the approved replay horizon;
do not replay dropped partitions into the live ledger without a separate rebuild.

Ingest counts increment at acceptance. Only stream seconds sum across days for a
monthly cap; ingest uses today's row.
Cache bytes are the live grant gauge; concurrent streams are pool-DO leases;
library size is distinct per-user membership (ADR 0004). Analytics Engine is
observability only. Duplicate queue delivery must not increment any flow twice.
Acceptance must cover concurrent duplicate delivery, crash before/after commit,
missing partitions, concurrent ingest increments, stream-then-ingest and
ingest-then-stream ledger-row creation (both counters remain numeric and increment
once), and midnight/month boundaries. These are required implementation tests, not
executed DB tests in this docs PR.

Sources: PostgreSQL [partitioned unique constraints](https://www.postgresql.org/docs/current/ddl-partitioning.html#DDL-PARTITIONING-DECLARATIVE-LIMITATIONS)
and [INSERT / ON CONFLICT / RETURNING](https://www.postgresql.org/docs/current/sql-insert.html),
checked 2026-10-15. `RETURNING` includes only successfully inserted/updated rows.

### 6. Catalog identity and playlist entries

The global catalog represents release-track occurrences, **not** one row per
recording. `artists.mbid` is an MB **artist** id; `releases.mbid` is an MB
**release** id, not release-group id; `tracks.mbid` is an MB **recording** id.
A recording may occur on multiple releases or twice on one release, so its MBID
is deliberately not unique. A track's canonical key is
`(release_id, medium_no, track_no)`: both integers are 1-based ordinal positions,
not display labels such as `A1`. This distinguishes multidisc tracks and repeated
recordings. MusicBrainz describes a track as a recording's occurrence on a
particular release/medium ([track definition](https://musicbrainz.org/doc/Track)).

The currently merged MB mirror has only one nullable `release_mbid` and nullable
`track_no` on `mb_recording`; it does not represent per-release occurrences or
medium positions. A NULL mirror `track_no` is unknown, never an implicit 1. Until
the mirror follow-up supplies a per-release tracklist with `(release_mbid,
medium_no, track_no, recording_mbid)` and an MB worker read projection, do not
match imported files to global track occurrences using that incomplete mirror.
Keep them under a local release/track key derived from the immutable manifest,
retaining a known release ID as `release_mbid_hint`; match globally only when the
mirror or accepted manifest supplies the exact occurrence position.

For artists/releases, CHECK exactly one of `mbid` and `local_key` is non-NULL.
Matched writes upsert on the partial unique MBID indexes (`ON CONFLICT (mbid)
WHERE mbid IS NOT NULL`); unmatched writes upsert on `local_key`. The stable local
key is a versioned length-prefixed tuple `(ingest_job_id, entity_kind,
manifest_group_first_entry_ordinal)`, derived from the immutable accepted manifest:
entries share an artist or release only when they share that manifest's explicit
entity-group reference, and the ordinal is the group's lowest 1-based entry ordinal.
This makes multi-album manifests deterministic across retries without merging by
artist/title text. For an unmatched release's tracks use `medium_no=1` and the
manifest's 1-based file-entry ordinal as `track_no`. Separate unmatched jobs may
remain distinct until explicitly matched. A release with a known MBID but ambiguous
track-position association remains local and stores that ID only in
`release_mbid_hint`; the hint is non-unique, is not a canonical MB join key, and is
retained for later explicit reconciliation.

The ingest `write` transaction upserts artist, release, then tracks in ascending
`(medium_no, track_no)` order; every multi-row statement orders its input rows by
that same key. It uses their **returned local ids**, including on conflicts, for
membership and per-user sources. Different jobs/users matching the same release
position converge to one track; `(library_id, track_id)` makes repeated membership
a no-op. A contradictory non-NULL recording MBID at an existing position is a
match error for reconciliation, not permission to overwrite shared metadata or
create a second track. Retry the entire idempotent write transaction on SQLSTATE
`40P01` or `40001`, with a bounded maximum of three attempts and backoff; after
exhaustion fail the job for normal retry/dead-letter handling.

For a user's second import of the same shared track, `track_sources` remains one
slot per `(user_id, track_id)`. Compare jobs by `(ingest_jobs.accepted_at,
source_job_id)`; under the source-row conflict lock, only a newer accepted job
may replace the encrypted source. In that same transaction, superseding the source
makes older `track_media` rows ineligible
for serving and removes that user's stale media mapping; remove its cache grant
only when no other current `track_media` mapping for that user/hash needs it. Shared
cache objects remain while any grant references them. Associate each verified media
mapping with its `source_job_id`. Serve only mappings for the current source job
that also have the user's grant; when several hashes exist for that job, choose
latest `verified_at`, then ascending `content_hash` as a deterministic tie-break.
This preserves a usable source and an unambiguous media choice after retries or
re-imports. Local-to-MB consolidation is separate transactional work: preserve/remap
membership, playlists and user sources before retiring a duplicate; don't mutate
keys on conflict or discard data. An MBID is never a local track id/source FK.

Playlist entries have their own ids so the same track may occur repeatedly.
The deferrable `(playlist_id, position)` constraint allows transactional position
shifts/reorders.

Implementation acceptance covers concurrent cross-user and same-user/different-job
imports of one matched release position (one global row, one library count), with
the newer accepted same-user job replacing the source and invalidating only its
stale per-user media/grant mapping; serving selects only a current-source hash and
uses the documented deterministic tie-break. Include opposite-order multi-track
inputs to prove sorted upserts and bounded retries. Cover the same recording on
two releases and two positions on one release (distinct tracks), multidisc positions,
NULL mirror `track_no` remaining unknown/local, unmatched-job retry, deterministic
artist/release grouping across a multi-album manifest retry, and an ambiguous match
retaining its `release_mbid_hint` without guessing.

### 7. Existing migrations stay at their applied paths

Main has `migrations/0001_mb_mirror.sql` (mirror) and
`migrations/0002_auth_vault.sql` (app), both **top-level**, and no migration runner.
This ADR keeps those paths and withdraws the draft's `migrations/{app,mb}/` move.
New migrations are forward-only, uniquely numbered top-level files with an
explicit app/mirror target in the reviewed runner manifest. Never glob every SQL
file into both databases. The future runner must keep per-database applied-file
checksums and reject drift; a directory name or file prefix alone is not routing.

No applied migration is rewritten or moved. `CREATE TABLE IF NOT EXISTS` does
not alter types, constraints or columns in existing tables. New app tables use
TEXT user FKs from the outset; no auth type conversion is required. Deploying
a migration runner/manifest and staging upgrade tests is separate implementation
work. Production DB creation/migration remains gated by the ops contract.

## Concrete implementation alignment (not already deployed)

- **Auth/vault (#15 merged):** retain every auth table/counter above and legacy
  `provider_vaults`. Multi-provider support is additive: create `providers` and
  `indexers` with v2 envelopes, read/decrypt v1 through its existing code, then
  re-encrypt into row/field-bound v2 envelopes. Do not byte-copy v1 ciphertext
  under v2 AAD, discard the legacy row, or switch readers before staged
  backfill/dual-read parity and independent security acceptance. The old
  single-vault path remains usable until that follow-up lands.
- **Ingest (#10 merged):** `workers/ingest/src/library.rs` currently upserts
  singular `library(idempotency_key, user_id, release_mbid, recording_mbids[])`;
  no table migration exists on inspected main. The implementation follow-up
  must inspect staging rather than assume this table exists, add normalized
  tables, and change the post-match writer. For any existing legacy rows,
  preserve idempotency/history and backfill local track ids only with an
  explicit per-file match; ambiguous recording arrays retain NULL MBIDs/source
  manifests rather than guessing associations. Keep legacy data/read support
  until parity is verified; never use create-if-absent as a conversion.
- **MusicBrainz mirror (merged):** the current `mb_recording` shape cannot provide
  the per-release medium/track occurrence required by this catalog key. Revise
  ADR 0007 and add a forward-only mirror migration for a tracklist relation keyed
  by `(release_mbid, medium_no, track_no)`, with `recording_mbid` as its reference;
  expose the projection through `workers/mb`. Until that follow-up ships, matching
  uses only exact positions already present in the immutable accepted manifest;
  otherwise retain local catalog ids and `release_mbid_hint` as specified above.
- **Stream/Subsonic/billing:** implement the target writers, envelope/pepper
  constraints, grant checks, budgets and lifecycle with their contract tests.
  These changes are not implied by merging this document. Existing in-flight
  work may continue, but cannot claim alignment until its implementation matches.
- **Migration/release:** explicit DB targets, staging upgrade from the existing
  auth schema, replay/constraint checks, and rollback/read compatibility precede
  release. No production migration, data removal or key rotation is done here.
