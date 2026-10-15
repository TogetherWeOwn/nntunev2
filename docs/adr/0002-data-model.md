# ADR 0002: Data Model + ERD (PlanetScale Postgres via Hyperdrive)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — preserve merged auth; pin catalog identity, atomic
  stream accounting, relational price mappings and bounded renewal settlement;
  define the audio-manifest producer and mirror-only shared-catalog authority;
  add library-scoped catalog reads, same-job dedup, and text/compilation grouping;
  gate position-collapse on identity, define candidate sets with dominant-artist
  rule, unify playlist/library predicates with dormant entries, add release_art
  cover-art path, and specify fenced account deletion, FK order and local-catalog GC.
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
`audit_log.user_id` and `audit_log.source_ip` stay `NOT NULL` per the applied
migration (pseudonymization uses the fixed sentinels `'deleted'` /
`'redacted'`, never NULL — rule 3); `audit_log` lookups reuse the existing
`idx_audit_user`, so no additive audit index is created. Only
`auth_challenges` gains an additive `INDEX (user_id)` for account completion,
which deletes `auth_challenges` rows by `user_id` and pseudonymizes
`audit_log` rows by `user_id` (rule 3).

### Additive target tables — implementation work remains

Unless stated otherwise, new ids are UUID, timestamps are `timestamptz`, and
`user_id` is TEXT FK → users. Encryption field groups are expanded in rule 1.
`users.deleting_at timestamptz NULL` is an additive column on the retained auth
schema, not part of the already-applied auth migration.

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
libraries             id PK, user_id FK, name, created_at, deleting_at NULL
artists               id PK, mbid UUID NULL (MB artist), local_key TEXT UNIQUE NULL,
                      local_job_id UUID NULL FK → ingest_jobs (indexed owner path;
                      NOT NULL when local_key present),
                      name, partial UNIQUE (mbid) WHERE mbid IS NOT NULL,
                      INDEX (local_job_id) WHERE local_job_id IS NOT NULL
releases              id PK, mbid UUID NULL (MB release), local_key TEXT UNIQUE NULL,
                      local_job_id UUID NULL FK → ingest_jobs (indexed owner path;
                      NOT NULL when local_key present),
                      release_mbid_hint UUID NULL (non-canonical reconciliation hint),
                      artist_id FK, title, release_type, year,
                      partial UNIQUE (mbid) WHERE mbid IS NOT NULL,
                      INDEX (local_job_id) WHERE local_job_id IS NOT NULL
tracks                id PK, mbid UUID NULL INDEXED (MB recording, NOT unique),
                      release_id FK NOT NULL, title, medium_no INTEGER NOT NULL,
                      track_no INTEGER NOT NULL, duration_ms,
                      UNIQUE (release_id, medium_no, track_no)
release_art           release_id FK → releases ON DELETE CASCADE,
                      rendition TEXT NOT NULL DEFAULT 'original',
                      source_kind TEXT NOT NULL ('embedded'|'caa'),
                      content_hash TEXT NULL FK → cache_objects ON DELETE RESTRICT,
                      last_served_at NULL, retry_after NULL,
                      last_failure TEXT NULL ('unavailable'|'transient'),
                      transient_failures SMALLINT NOT NULL DEFAULT 0
                        CHECK (transient_failures >= 0 AND transient_failures <= 3),
                      created_at, updated_at,
                      PK (release_id, rendition), -- durable source, nullable cache pointer
                      INDEX (content_hash) WHERE content_hash IS NOT NULL
                        -- sweep step 5 mark test
library_tracks        library_id FK, track_id FK, added_at, PK (library_id, track_id),
                      INDEX (track_id) -- GC reference test + track-delete RI path
track_sources         user_id FK, track_id FK, source_job_id FK → ingest_jobs,
                      indexer_id FK NULL, nzb_ref_encrypted BYTEA
                      + nzb_ref_* envelope fields, PK (user_id, track_id),
                      INDEX (track_id), -- cross-user track-delete RI path
                      INDEX (source_job_id) -- ingest_jobs-delete RI path
track_media           user_id FK, track_id FK, source_job_id FK → ingest_jobs,
                      content_hash FK → cache_objects, format TEXT, bitrate INTEGER,
                      verified_at, last_served_at,
                      PK (user_id, track_id, source_job_id, content_hash),
                      INDEX (track_id), -- GC/track-delete RI path
                      INDEX (source_job_id), -- ingest_jobs-delete RI path
                      INDEX (content_hash) -- sweep step 5 mark test
user_cache_grants     user_id FK, content_hash FK → cache_objects,
                      provider_id UUID NOT NULL,
                      FK (user_id, provider_id) → providers(user_id, id) ON DELETE RESTRICT,
                      verified_at, last_served_at, PK (user_id, content_hash),
                      INDEX (content_hash) -- sweep step 5 mark test
cache_objects         content_hash TEXT PK (sha256 hex), byte_size BIGINT,
                      r2_bucket TEXT, r2_key TEXT,
                      ref_count BIGINT (advisory), created_at, last_verified_at
playlists             id PK, user_id FK, name, created_at, deleting_at NULL
playlist_tracks       id PK, playlist_id FK, track_id FK, position INTEGER,
                      UNIQUE (playlist_id, position) DEFERRABLE INITIALLY DEFERRED,
                      INDEX (playlist_id, track_id),
                      INDEX (track_id) -- GC reference test + track-delete RI path
step_up_proofs        id UUID PK, user_id TEXT NOT NULL FK → users ON DELETE CASCADE,
                      session_id TEXT NOT NULL, action TEXT NOT NULL
                        ('delete-account'|'cancel-deletion'),
                      method TEXT NOT NULL ('passkey-uv'|'magic-link'),
                      token_hash TEXT NULL, -- magic-link only: SHA-256 hex of the
                        -- step-up link token, set at begin; NULL for passkey-uv
                      challenge_id TEXT NULL, -- passkey-uv only: WebAuthn challenge
                        -- id issued at begin; NULL for magic-link
                      CHECK ((method = 'magic-link') = (token_hash IS NOT NULL)),
                      CHECK ((method = 'passkey-uv') = (challenge_id IS NOT NULL)),
                      issued_at timestamptz NOT NULL,
                      expires_at timestamptz NOT NULL
                        CHECK (expires_at = issued_at + INTERVAL '15 minutes'),
                      verified_at timestamptz NULL, consumed_at timestamptz NULL,
                      attempts SMALLINT NOT NULL DEFAULT 0
                        CHECK (attempts >= 0 AND attempts <= 5),
                      partial UNIQUE (session_id, action)
                        WHERE consumed_at IS NULL
                      -- Action-bound single-use step-up (ADR 0006): begin runs in
                      -- one transaction that first retires any prior live row for
                      -- the same (session_id, action) — UPDATE ... SET
                      -- consumed_at = now() WHERE session_id = $s AND action = $a
                      -- AND consumed_at IS NULL (covers expired, abandoned, and
                      -- method-switch supersedes) — then inserts the new row, so
                      -- an expired proof never blocks re-begin. begin is
                      -- rate-limited to 5/hour per (session_id, action) and
                      -- complete to 10/15min per proof id; attempts caps at 5,
                      -- then the proof must be re-begun. The consume key is always
                      -- the proof id returned from begin
                      -- (WHERE id = $1 AND ... in both ADRs).
                      -- begin issues the challenge/link with purpose
                      -- 'step-up:<action>:<proof_id>' and stores its token_hash /
                      -- challenge_id here; complete requires that exact hash/id
                      -- (WHERE id = $1 AND token_hash = $t for magic-link, or
                      -- challenge_id = $c for passkey-uv, plus the existing
                      -- verified/unconsumed/session/action/expiry predicates) and
                      -- marks the underlying token used / challenge consumed.
                      -- Step-up magic links are claimed only at the dedicated
                      -- step-up complete path, which never creates a session; the
                      -- ADR 0008 login claim path never verifies a proof. Login
                      -- links (no bound hash) and login challenges (no bound id)
                      -- therefore never verify a proof. Complete verifies the
                      -- assertion/claim and sets verified_at (attempts++ per
                      -- try). DELETE/cancel-deletion consumes
                      -- atomically via UPDATE ... SET consumed_at=now()
                      -- WHERE id=$1 AND session_id=$session AND action=$action
                      -- AND verified_at IS NOT NULL AND consumed_at IS NULL
                      -- AND expires_at > now() RETURNING; zero rows rejects.
                      -- Existing magic_link_tokens/auth_challenges/sessions gain
                      -- NO purpose/action column; the binding lives here via
                      -- token_hash/challenge_id.
delete_operations     id UUID PK, user_id TEXT NOT NULL FK → users ON DELETE RESTRICT,
                      scope TEXT NOT NULL ('account'|'library'|'playlist'),
                      container_id UUID NULL, cursor JSONB NOT NULL DEFAULT '{}',
                      state TEXT NOT NULL DEFAULT 'pending'
                        CHECK (state IN ('scheduled','pending','running','failed','done')),
                      erase_after timestamptz NULL
                        CHECK ((scope='account') = (erase_after IS NOT NULL)),
                      heartbeat_at timestamptz NOT NULL DEFAULT now(),
                      consecutive_failures SMALLINT NOT NULL DEFAULT 0
                        CHECK (consecutive_failures >= 0),
                      last_error TEXT NULL,
                      started_at timestamptz NOT NULL,
                      CHECK ((scope='account') = (container_id IS NULL)),
                      partial UNIQUE (user_id) WHERE scope='account',
                      partial UNIQUE (user_id, scope, container_id)
                        WHERE scope IN ('library','playlist')
                      -- state/heartbeat/failures advance in the same transaction
                      -- as the cursor; GET /v1/operations/{id} is owner-scoped
                      -- (caller user_id must match) and reports scheduled/
                      -- pending/running/failed from these columns;
                      -- library/playlist 'done' rows
                      -- are retained 7 days then swept; account completion removes
                      -- all of that user's rows and confirms by email (0006).
                      -- Account rows are created 'scheduled' with
                      -- erase_after=requested_at+24h; library/playlist rows have
                      -- erase_after NULL. The hourly delete-sweep resumes only
                      -- rows with state IN ('pending','running','failed') whose
                      -- heartbeat_at is older than one hour; 'scheduled' rows are
                      -- never matched by that predicate. Erasure starts only via
                      -- UPDATE delete_operations SET state='running',
                      -- heartbeat_at=now() WHERE id=$1 AND state='scheduled'
                      -- AND erase_after <= now() RETURNING; the sweep issues this
                      -- conditional start (no 24h-sleeping Workflow with a stale
                      -- heartbeat). POST /v1/account/cancel-deletion wins the
                      -- boundary by the same compare-and-set (UPDATE ... WHERE
                      -- state='scheduled'; zero rows means erasure already
                      -- started, return 409): exactly one of cancel vs executor
                      -- start succeeds
play_event_outbox     event_id UUID PK, user_id FK, track_id FK,
                      played_at timestamptz NOT NULL, stream_seconds BIGINT NOT NULL,
                      cached_hit BOOLEAN, delivered_at NULL,
                      CHECK (stream_seconds >= 0),
                      INDEX (user_id, played_at), INDEX (track_id)
play_events           event_id UUID NOT NULL, user_id FK, track_id FK,
                      played_at timestamptz NOT NULL, stream_seconds BIGINT NOT NULL,
                      cached_hit BOOLEAN, PK (event_id, played_at),
                      CHECK (stream_seconds >= 0),
                      PARTITION BY RANGE (played_at), monthly,
                      PARTITIONED INDEX (user_id, played_at),
                      PARTITIONED INDEX (track_id)
deleted_billing_tombstones id UUID PK, stripe_customer_id TEXT NULL,
                      UNIQUE (stripe_customer_id) WHERE stripe_customer_id IS NOT NULL,
                      email_hmac TEXT NOT NULL, pepper_version INTEGER NOT NULL,
                      INDEX (pepper_version, email_hmac),
                      trial_started_at timestamptz NULL (write-once copy),
                      open_invoice_refs JSONB NOT NULL DEFAULT '[]',
                      debt_refs JSONB NOT NULL DEFAULT '[]', deleted_at timestamptz NOT NULL
                      -- no FK to users; skipped (no row) when stripe_customer_id
                      -- IS NULL and no trial/debt history exists. email_hmac is
                      -- HMAC-SHA256 of the normalized email (trim, lowercase,
                      -- strip plus-tags) under the BILLING_EMAIL_PEPPER
                      -- versioned keyring (Worker secret: current_version plus at
                      -- most one previous_version with a version→key map, v1 at
                      -- launch; same shape as SUBSONIC_INDEX_PEPPER in rule 2).
                      -- Trial lookup tries every retained pepper version against
                      -- the composite (pepper_version, email_hmac) index. A
                      -- version is retired only after no tombstone row references
                      -- it within the 36-month retention; an HMAC cannot be
                      -- rehashed without the plaintext, so early retirement
                      -- would silently end trial single-use for those rows.
                      -- Retained 36 months for trial single-use and invoice/debt
                      -- history, then deleted; the retention basis is trial-abuse
                      -- prevention plus invoice/debt reconciliation as stated
                      -- here. The raw email/name remain only on the
                      -- retained remote Stripe customer, never in this table
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
users ──< usage_ledger / ingest_jobs / play_event_outbox / play_events / delete_operations
users ──< libraries ──< library_tracks >── tracks >── releases >── artists
users ──< track_sources >── tracks
users ──< track_media >── tracks
track_media >── cache_objects <── user_cache_grants >── users
releases ──< release_art >── cache_objects
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
  Unmatched tracks have local ids and NULL MBIDs. This stage writes no media hash;
  it also inserts idempotent `release_art` source descriptors with NULL cache
  pointers, then checkpoints a continuation to the post-write art stage (ADR 0003).
- **Post-write `art` stage / cover-art cache refill:** acquire/validate art and put
  R2 bytes outside SQL, then publish the cache pointer in a short transaction
  (ADR 0003). Art is authorized by library membership, not audio grants.
- **Stream worker, after full own-provider fetch and hash verification:**
  `cache_objects`, per-user `track_media`, and `user_cache_grants`. Initialize
  both `last_served_at` columns and refresh the grant/served mapping on serve.

Audio eviction removes grants, then orphaned mappings, then unreferenced objects
(ADR 0003); it never removes sources. Final removal from all of a user's libraries
removes that user's media mappings and unneeded grants, not other users' copies.
Retain the encrypted `track_sources` while **any own library membership OR own
playlist entry** references that track. Delete the source only when both counts
are zero, including after playlist-entry/playlist deletion. Serialize membership,
playlist and source cleanup for `(user_id, track_id)` in one transaction; a
concurrent re-add must not lose its source. For a local release, every source
insert/delete/replace and every membership change takes the release serializer
first (ADR 0003), computes the selected art source (lowest track id among
current member tracks with `embedded_art_selector`) before and after inside
that lock, and in that same transaction clears `release_art.content_hash`,
`last_served_at`, `retry_after`, `last_failure`, `transient_failures` and
advances `updated_at` only when the selection changed. A mutation that leaves
the same selected source — for example removing one of several member tracks
while the lowest-track-id art source remains, including via final-reference
cleanup — leaves the pointer intact; removing/replacing the selected source
clears it. Do not keep serving old artwork after its source is gone.

Deleting a library, playlist, or account is an explicit fenced, resumable batched
procedure, never a bare FK cascade. The schema adds nullable `deleting_at` to
`users`, `libraries`, and `playlists`, plus `delete_operations` with a durable
batch cursor. Entry points are declared in ADR 0006: `DELETE /v1/account`,
`DELETE /v1/libraries/{id}` and `DELETE /v1/playlists/{id}` each return `202`
with an operation id; `GET /v1/operations/{operation_id}` reports its state.
The executor is the `delete` Workflow (Queues/Workflows, Containers-eligible
batch worker): it advances the persisted cursor in bounded batches, retries a
failed batch with backoff, alerts after three consecutive failures, and an hourly
`delete-sweep` job resumes operations with `state IN
('pending','running','failed')` whose `heartbeat_at` is older than one hour
(same pattern as the ADR 0005 `billing-reconcile`); rows in `scheduled` state
are excluded from that predicate and start only through the conditional
`erase_after <= now()` UPDATE above. Starting a scoped
delete atomically marks the relevant row and inserts its operation; the cursor
records the last completed key and advances in the same transaction as each
batch, together with `state`, `heartbeat_at`, `consecutive_failures` and
`last_error`. The delete cursor keys on indexed access paths only: `(user_id,
played_at, event_id)` for `play_events`/`play_event_outbox` via their
`(user_id, played_at)` indexes; `(library_id, track_id)` via
`libraries(user_id)` for membership batches; `(playlist_id, entry id)` for
playlist-entry batches; `(user_id, content_hash)` for grant batches; `track_id`
via the `library_tracks(track_id)`, `playlist_tracks(track_id)`,
`track_sources(track_id)` and `track_media(track_id)` indexes for the GC
reference test and every `DELETE FROM tracks` RI check;
`(source_job_id)` on `track_sources`/`track_media` for every `DELETE FROM
ingest_jobs` RI check; `(content_hash)` on `track_media`,
`user_cache_grants`, and partial `release_art` for the sweep step-5 mark test;
and `local_job_id` for
owner-local catalog rows. A repeat DELETE of the same account/container is
idempotent: while its operation row exists the API returns the existing
operation id with `202`, not a unique violation or a fence `409`. A crash
resumes that operation via the executor or the sweep, never only via a user
retry. A completed library/playlist operation marks its row `done` and removes
the container in one transaction; `done` rows are retained 7 days for status
reads, then swept. Account completion removes **all** of that user's
`delete_operations` rows (including in-flight scoped operations, which the
account sweep supersedes and cancels) and the user in one transaction, so no
`ON DELETE RESTRICT` operation row can block the final `DELETE FROM users`;
the erased account learns the result from the confirmation email (ADR 0006),
and a later `GET /v1/operations/{id}` for it is `404`.

Every writer that can create or restore user-owned state first takes
`SELECT ... FOR SHARE` on the user's `users` row and rejects a non-NULL
`deleting_at`; library/playlist mutations also take `FOR SHARE` on the target
container and reject its marker. This includes ingest `write`, membership and
playlist edits/restores, stream publication of `track_media`/grants (including
play-event/outbox writes), and art descriptor/pointer publication. The delete
executor's own billing reconcile (account gate below) and its cursor/state
writes are exempt from this fence. The auth/session writers needed to
cancel during grace — new `sessions` login rows,
`magic_link_tokens` issue/consume, `auth_challenges` challenge/assertion,
`webauthn_credentials.sign_count` updates, the `step_up_proofs` begin/verify/
consume writes, `audit_log` writes, session rotation, and
`POST /v1/account/step-up/begin`, `POST /v1/account/step-up/complete`, and
`POST /v1/account/cancel-deletion` — are exempt ONLY while the account
operation is in `state = 'scheduled'` (the only state where cancel-deletion
can win the compare-and-set). Once the operation is `running` (erasure
started) those writers are fenced like any other: they take `FOR SHARE` on
the users row, see the started operation, and fail closed with `409
account_deleting`; no new login, step-up, or real-user audit row can be
created after that point.
all other billing/subscription writers and every data writer
remain fenced once `deleting_at` is set. A fenced
writer fails closed with native `409 Conflict` carrying
`code: account_deleting` (or `container_deleting`), the blocking
`operation_id`, and a `retry_after` hint; it never queues behind the delete.
Deletion sets the corresponding marker with an
UPDATE (conflicting with writer locks), waiting for already-started writers; no
writer can pass a marker check and commit after the marker. Publication holds
these locks through its inserts. Stream hash serialization and the release
serializer remain additional locks, not substitutes for the user/container
fence. Re-enumerate memberships, grants, and media under the relevant serializers
until empty before deleting providers or jobs; the fence prevents late
publication from repopulating any of them. FKs on `library_tracks`/`playlist_tracks`
remain RESTRICT/non-cascading.

For library/playlist batches, enumerate affected pairs and local release ids,
take serializers in ascending release id, then `(user_id, track_id)`, then hash
order (release-then-hash per ADR 0003). In each transaction, delete the target
membership/entry rows, delete `track_media` only for tracks with no remaining
library membership, delete grants no other current mapping for that user/hash
needs, run both-counts-zero source cleanup, and conditionally invalidate local
`release_art` only when its selected source changes. Re-enumerate until the
container is empty before removing its marker/container; the persisted cursor is
a resume hint, not authority to skip rechecking.

Account deletion first marks `users.deleting_at` and records the operation as
`scheduled` with `erase_after = requested_at + 24h`. For accounts the erasure
executor starts only after the 24-hour cancellable grace window in ADR 0006
via the conditional `state='scheduled' AND erase_after <= now()` UPDATE; the
hourly sweep's heartbeat-staleness resume never matches `scheduled` rows, so a
sweep at +1h does not start erasure. Cancel during grace requires a fresh login
and step-up when the session has lapsed (ADR 0006; sessions keep their existing
one-day absolute cap from ADR 0008 and are not extended by the grace window);
the sweep deletes sessions only when erasure begins.
Enumerate the user's own `ingest_jobs` and their `local_job_id` owner index
before deleting jobs; this indexed owner path (not `local_key` parsing) drives
local catalog cleanup, independent of current memberships/sources. In batches
keyed on the `(user_id, played_at)` event indexes remove `play_event_outbox`,
the user's monthly `play_events`, and `usage_ledger` rows before deleting
referenced tracks or the user; account erasure is the first explicit exception
to append-only event retention (rule 5). Then remove playlist entries and
library memberships, media, grants and sources. Remove owner-local catalog rows
and `release_art` descriptors with no remaining library/playlist references
under the release serializer. Billing is deleted only after remote termination is gated. The gate runs under
the same per-user serialized reconcile as ADR 0005 (exempt from the fence
above): first resolve every unresolved `billing_operations` row for the user,
then list ALL non-terminal remote subscriptions for the stored customer id
(never only the locally known row), cancel each immediately — not at period
end — with the pinned delete-cancel contract (`DELETE
/v1/subscriptions/{id}` with `prorate=false`, `invoice_now=true`,
`cancellation_details[comment]=account_deletion` as the deletion marker
(`reason` is response-only, never sent); no refund in this step;
ADR 0005; `proration_behavior=none` belongs only to the update call).
Cancelling stops automatic collection of open finalized invoices, so the gate
leaves them uncollected and records their refs in the tombstone's
`open_invoice_refs`/`debt_refs` for out-of-band reconciliation (no silent
retry, no recreation of the user). Confirm every one
reached terminal status (`canceled`/`incomplete_expired`) remotely and
persisted locally before any billing row is deleted. Then copy a minimal
tombstone into `deleted_billing_tombstones` (`stripe_customer_id` when
non-NULL, `email_hmac`, `trial_started_at` write-once copy, open invoice/debt
refs; skipped only when there is no customer id and no trial/debt history)
before deleting local billing operations/subscriptions/accounts. The Stripe
customer is retained remotely for invoice/debt history, never deleted with the
account; a webhook for an unknown or tombstoned customer that still shows a
live remote subscription cancels it immediately and alerts, otherwise it
reconciles debt without recreating a user and alerts. This preserves ADR 0005
trial single-use (`trial_started_at` survives as the tombstone copy) and the
rule 4 ban on freeing a slot while remote billing can still collect. Then remove ingest
jobs and empty library/playlist containers, providers/indexers/vaults, Subsonic
credentials, sessions, magic-link/WebAuthn rows and other user-FK children;
then, in the same transaction as the final `DELETE FROM users` after taking
`SELECT ... FOR UPDATE` on the users row (auth writers hold `FOR SHARE`, so
they serialize against it), `DELETE FROM auth_challenges WHERE user_id=$1`
(via the additive index)
and pseudonymize `audit_log` rows for that `user_id` to the fixed sentinels
(`user_id='deleted'`, `source_ip='redacted'`, satisfying the migration's
`NOT NULL` constraints — never NULL), excluding the final deletion row by id:
first insert the retained row with `user_id='deleted'`,
`action='account_deleted:<operation_id>'`, `source_ip='redacted'`,
`ok=TRUE`, `at=<completion time>`, then run `UPDATE audit_log SET
user_id='deleted', source_ip='redacted' WHERE user_id=$1` (which no longer
matches the already-sentinel final row). A login attempt that lands during
`running` therefore fails `409 account_deleting` before writing, and no
real-user audit/challenge row can appear after these statements. The final row is retained 13 months,
then swept by the daily `audit-retention-sweep` (`DELETE FROM audit_log WHERE
action LIKE 'account_deleted:%' AND at < now-13mo`, via a partial index on
`(action, at)` for that prefix) — no user-keyed audit/challenge row
survives except that one retained deletion event; remove **all** of that
user's `delete_operations` rows and, last, the account operation and user.
Honor each actual FK in the migration manifest's order; do
not rely on cascade. The job ids and `local_job_id` values remain available until
the local sweep completes. Periodic owner-keyed local-catalog GC enumerates local
rows via the indexed `artists.local_job_id` / `releases.local_job_id`
(not by parsing `local_key`; new local writes populate both, backfilled once)
and removes rows with no library/playlist references. Per-track event erasure
is the second explicit exception to append-only: each GC batch first deletes
that batch's `play_event_outbox`/`play_events` rows for its orphan track ids
(using the `(track_id)` indexes) in the same transaction before `DELETE FROM
tracks`, so a played-then-removed local track cannot fail with 23503. Local-track
consolidation remaps `play_event_outbox`/`play_events` (plus memberships,
playlists and sources) to the surviving track before retiring a duplicate.
This covers rows orphaned by earlier final-membership removal as well as
interrupted account deletion, and runs before those jobs are eligible for
cleanup. Periodic orphan-source reconciliation (ADR 0003 GC) removes
`track_sources` with neither library nor playlist references. A retained
source is recovery data, not read/stream authorization: dormant entries still
fail the library gate. A live audio grant is required even when an object or
track id is known.
`cache_bytes` is a live SUM of object sizes across that user's unique audio grants,
not a daily/monthly flow; artwork uses the separate lifecycle in ADR 0003.

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

`play_events` is append-only for normal operation and monthly-partitioned, with exactly two deletion exceptions, both defined in rule 3: (a) account erasure, which deletes that user's outbox and event rows before deleting tracks/users; (b) periodic owner-keyed local-catalog GC per-track erasure, which deletes only the orphan track ids' outbox/event rows in the same batch transaction before deleting those tracks, plus consolidation remap of outbox/events to the surviving track. Other retention/drop policy requires its own approved operation, not this ADR. Partition maintenance creates future partitions and alerts if one is missing; no silent dropped writes.
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
medium positions. A NULL mirror `track_no` is unknown, never an implicit 1.
**Only the MB worker's trusted mirror projection may authorize a global release
and track occurrence.** It must supply `(release_mbid, medium_no, track_no,
recording_mbid)` and the associated MB artist/release/track metadata. File tags,
NZBs, indexer results and manifest claims are untrusted hints, never authority to
create a shared position. `match` must resolve an unambiguous occurrence in that
projection and verify any claimed recording MBID against it; a release MBID alone
is insufficient. Shared artist names, release titles and track titles/durations
come from that MusicBrainz projection, **never tenant file tags**. Unknown MB
metadata stays NULL where allowed; it is not filled from tenant metadata. This
preserves MusicBrainz's distinction between a release track and its recording.
Until both the manifest producer below and the mirror occurrence projection ship,
all normalized catalog writes are local; the existing flat recording array is
not an occurrence match. Keep any known release ID only as `release_mbid_hint`.

**Accepted audio manifest (target contract, not present on inspected main).** Add
a tag/fingerprint subphase at the start of Workflow `match` / `JobState::Matching`,
after verification/repair but **before occurrence matching**. This producer writes
`accepted_audio_manifest_v1` into an extended durable `JobCheckpoint` before any
MB lookup. No existing `tag` step or `Tagging` state is implied. "Accepted" means
structurally validated and frozen, not trusted catalog metadata. It contains the
authenticated `job_id`/`user_id`, a version, and an ordered entry per selected audio
file. Each entry carries:

- `audio_ordinal`: unique, contiguous, 1-based over **selected audio files only**;
  PAR2, NFO and other non-audio files never consume an ordinal.
- `source_selector`: exact NZB file/ordered segments, plus archive-member/audio
  selection where applicable, sufficient to reconstruct that audio file later.
- Optional `embedded_art_selector`: tag container, picture-block ordinal and
  declared MIME type within that selected audio file, frozen by the tag producer.
  While those bytes are already in hand the producer also extracts the indicated
  picture, validates it (JPEG/PNG/WebP, at most 5 MiB and 16 megapixels per ADR
  0003), stages the validated bytes in the environment art bucket keyed by
  content hash (R2 PUT outside SQL; an unreferenced staged object is removed by
  the 24-hour orphan sweep), and checkpoints only `(audio_ordinal,
  content_hash, byte_size, mime)` per entry, deduplicated by hash so one shared
  album cover checkpoints once. No image bytes and no tag-header snapshot are
  stored in the `JobCheckpoint` (Workers KV 25 MiB value / 1 MiB Workflow
  step-result limits). The producer fetches at most an 8 MiB tag-header prefix
  per audio file and at most 32 MiB total per job for this extraction; a file
  that cannot yield its picture within the per-file cap is recorded as
  no-embedded-art. Copy the content hash with `source_selector` into the
  encrypted lifetime `track_sources` payload. It locates bytes for later
  validation, never an arbitrary URL or grant.
- Separate job-local `artist_group_ref` and `release_group_ref`, so one artist can
  span several releases. Each release group refers to one artist group in this
  model. These are catalog grouping references, **not NZB `<group>` newsgroups**.
- Optional claimed artist/release/recording MBIDs, disc and track numbers, file
  tags/fingerprint results, and the frozen local position derived below. All
  claims remain hints even when syntactically valid.

The producer orders audio selections by original NZB file order, then archive
member path in bytewise order and audio-selection index, assigns ordinals once,
and freezes grouping and selection with them. Grouping is job-local only (the
local key embeds the job id, so it cannot merge another tenant's rows); MBIDs are
the stronger signal, normalized text is the fallback. Apply grouping in this order:

1. Across the **whole job**, entries with the same syntactically valid non-NULL
   claimed release MBID form one release group, regardless of differing or absent
   album-title tags. Different release MBIDs never merge just because titles match.
   The artist group key is the claimed album-artist MBID when present, else the
   claimed track-artist MBID. If those artist keys differ within a release-MBID
   group, put that release under the job-local Various-Artists holder; per-track
   credits remain in the manifest tags, not normalized artist rows. Missing artist
   keys use the text/dominant-artist rule below within that release group, without
   splitting the release. A release MBID is still only a local grouping hint until
   the trusted mirror resolves an occurrence.
2. Only entries **without a release MBID** enter text grouping. Here **candidate
   set** means entries sharing one nonempty normalized album-title text within
   `(user_id, job_id)`. Partition those entries by title, so album X by A and album
   Y by B group independently. Never attach a missing-MBID entry to an MBID group
   by title alone. Normalization is trim, case-fold and collapse internal whitespace.
   Group by normalized (album-artist text, album-title text); album-artist text
   means albumartist/TPE2 when present, else a single common track-artist text
   across that candidate set. Equal text pairs form one release/artist group.
   Without TPE2 or a single common track artist, strip case-insensitive trailing
   ` feat. X`, ` ft. X`, ` featuring X`, ` with X` suffixes and count stripped
   texts. A nonempty artist covering at least two-thirds of the set holds the
   release; otherwise use the job-local Various-Artists holder. Per-track credits
   stay in the manifest. There is no minimum distinct artist count: a two-artist
   split, duet or 11-plus-1 `feat.` album without TPE2 forms one release.
3. Only when **both release MBID and nonempty album-title text are absent** use
   separate release groups per entry. An artist MBID alone does not identify an album.

Disc is never a group key; it determines `medium_no` only after grouping. These
rules group local hints, not authenticated MBIDs. `match` and `write` consume the
checkpoint on retry, never regenerate its ordinals/groups from changed tags.
After `match` resolves each entry to an occurrence (mirror-verified or local) and
before checkpointing classifications, collapse entries resolving to the same
occurrence within this job in two ordered passes. First derive an **effective
disc** per release group: if no entry carries a disc tag, use disc 1 for all its
entries (ordinary single-disc rips). If any entry carries a disc tag, each entry
needs its own positive disc value; absent, malformed or nonpositive values remain
missing. An absent/invalid track number is always missing. Freeze effective discs
in the manifest before dedup; all references to claimed pairs below use this
normalization. It never supplies a missing **mirror** position or authorizes a
global match. A positive `(disc, track)` pair alone never collapses anything:
it remains an untrusted hint. First, within each release group, consider entries
with the same positive effective `(disc, track)`: collapse
them only when identity also agrees — for mirror-verified entries only when
`match` resolved both to the same occurrence key, for local entries only when
the claimed pair is equal AND (normalized titles agree AND durations agree
within 2 seconds or 1%, whichever is larger, OR fingerprints are equal when both
present, OR claimed recording MBIDs are equal and non-NULL). Any other same-pair
entries are contradictory pairs among distinct occurrences, not duplicates, and
fall through to the whole-group ordinal fallback below so no file is dropped.
The winner of a true duplicate set is the lowest `audio_ordinal` (format
preference is not a tie-break; ordinals are unique); checkpoint losers as
`duplicate_of` and surface the loser list in the job result shown to the user.
Only then decide the frozen local positions for the surviving distinct
occurrences: claimed-position duplicates collapsed above never trigger the
whole-group fallback by themselves, but any missing pair or any contradictory
same-pair among distinct survivors sends the whole release group to the
`(1, audio_ordinal)` fallback (see the local-track rule below). Second, group
the survivors by the resolved (release key, medium_no, track_no) — mirror
occurrence for verified matches, manifest-derived local key for local matches —
and keep exactly one winner per group, the lowest `audio_ordinal`. Record losers
in the checkpoint as `duplicate_of` the winner for diagnostics and list them in
the job result; they write no separate track, source, membership or media row.
The winner's `source_selector` is the `track_sources` source for that
occurrence. After `match` and before building any `write` statement, also
collapse artist groups by resolved artist key and release groups by resolved
release key (`mbid` for verified rows, `local_key` for local rows), merging
colliding groups under one winner so untrusted hint variants
(`The Beatles`/`Beatles, The`, `Abbey Road`/`Abbey Road (Remastered)`, or two
claimed MBIDs resolving to one mirror row) cannot emit two rows with one key.
No multi-row statement in `write` may
contain a repeated key for `artists (mbid)` and `releases (mbid)` on their
partial unique MBID indexes, for `artists (local_key)` and `releases (local_key)`
on their unique local-key indexes, or for `tracks (release_id, medium_no, track_no)`,
`track_sources (user_id, track_id)` or `library_tracks (library_id, track_id)`.
SQLSTATE 21000 (duplicate key within one statement) is a deterministic bug, never
retried: fix the dedup, do not add it to the retry set.
Before `Writing`, also checkpoint per-entry match classifications (including the
dedup winners/losers above) and the trusted
mirror metadata snapshot used; a write retry does not rematch against a changed
mirror. Retain the checkpoint through job retries/replay; the `write` transaction
copies the relevant source selection into the lifetime encrypted `track_sources`,
not merely a checkpoint URL or expiring KV reference.

For artists/releases, CHECK exactly one of `mbid` and `local_key` is non-NULL.
Mirror-verified writes upsert on the partial unique MBID indexes (`ON CONFLICT
(mbid) WHERE mbid IS NOT NULL`); local writes upsert on `local_key`. The stable
local key is a versioned length-prefixed tuple `(ingest_job_id, entity_kind,
manifest_group_first_audio_ordinal)`: use the lowest `audio_ordinal` in that entry's
artist group for an artist, or release group for a release. This makes multi-album
retries deterministic without conflating those two groupings. Every local
artist/release write also populates indexed `local_job_id` with that same job id
(plus a one-time backfill parsing existing keys); GC and sweeps query
`local_job_id`, never parse `local_key` at runtime. For local tracks,
first apply the identity-gated claimed-position collapse above, then use claimed
`(disc, track)` for the surviving distinct occurrences only when every survivor
in that release group has a positive, unique pair AND no same-pair group in that
release group held distinct identities; otherwise the whole group uses
`(1, audio_ordinal)` (a collapsed FLAC/MP3 duplicate set alone does not force
this — only a missing pair or a contradictory same-pair among distinct survivors
does). Freeze that choice in the manifest. These local positions **never authorize a global
match**. Separate unmatched jobs may remain distinct until explicitly matched.
A known but ambiguous or contradictory release claim stays only in the local
release's `release_mbid_hint`; the hint is non-unique, is not a canonical MB join
key, and is retained for later explicit reconciliation.

The ingest `write` transaction upserts artist, release, then tracks in ascending
`(medium_no, track_no)` order; every multi-row statement orders its input rows by
that same key. It uses their **returned local ids**, including on conflicts, for
membership and per-user sources. Different jobs/users matching the same verified
release position converge to one track; `(library_id, track_id)` makes repeated
membership a no-op. Tenant claims cannot overwrite shared metadata on conflict.
If a claimed recording MBID contradicts the mirror, or an existing shared row's
MBID contradicts the trusted occurrence, classify that entry as local: write it
under its manifest-derived local release/track key with NULL canonical MBIDs and
retain the release hint. Use trusted MB metadata when available, otherwise that
user's file tags only on the local rows. Complete the import's membership/source
writes for that local track; do not abort the import, overwrite the shared row, or
insert a second global occurrence. Record the conflict for separate reconciliation.
Retry the entire idempotent write transaction on SQLSTATE `40P01` or `40001`, with a
bounded maximum of three attempts and backoff; after exhaustion fail the job for
normal retry/dead-letter handling.

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

Catalog access is library-scoped by default. Every read or write that takes or
returns a catalog id — native `/v1/artists`, `/v1/releases`, `/v1/tracks`,
`/v1/search`, `/v1/playlists` (including embedded track rows), `/v1/plays`,
`/v1/library`, and Subsonic `getArtist`, `getAlbum`, `getSong`, `search3`,
`getPlaylist(s)`, `getCoverArt`, `stream`, `scrobble`, plus `getMusicDirectory`/
`getAlbumList2`/`getIndexes` and any directory/list/index call where it lists
catalog rows — resolves ids only through the caller's own libraries:
`libraries(user_id = caller) → library_tracks → tracks → releases → artists`.
A non-member artist/release/track id returns 404 (native) or Subsonic error 70;
`search3`/search returns only member rows. A new endpoint that takes or returns
a catalog id is denied until it declares how it applies this gate. Local-row ownership is derived via the indexed `local_job_id` →
`ingest_jobs.user_id`; that user is the sole owner, and the library-scope rule
already restricts reads to it. `local_key` parsing is legacy backfill only,
never a runtime access path.
Mirror-verified global rows (when they ship) use the same membership gate: shared
metadata is readable only for tracks in the caller's library. Audio additionally
requires its own-provider grant. **Artwork is membership-plus-entitlement-authorized metadata:**
there are no art grants and art never contributes to `cache_bytes`; refills
require current entitlement (rule 4 / ADR 0004) as well as membership. `release_art`
holds a durable source descriptor and a nullable cache pointer; ingest `write`
records the descriptor, then a separate idempotent **post-write `art` stage**
acquires and caches bytes (ADR 0003). Local releases use tag-embedded art selected
from an own current `track_sources` payload; verified global releases use only
Cover Art Archive via the trusted release MBID, never tenant artwork. The schema
has one row per `(release_id, rendition)`; launch supports only `original`, which
is therefore the primary. Other sizes/renditions are deferred, not extra rows or
unbounded transforms. `content_hash=NULL` means uncached, not unrecoverable;
ADR 0003 defines refill and FK-safe expiry.

The `coverArt` id in `getAlbum`, `getSong` and `search3` is the opaque
`coverart:{release_id}`, never the raw hash. `getCoverArt` checks release membership
and current entitlement **before** resolving the art descriptor, reading R2 or scheduling a refill. It
serves the original from `nntunev2-art-{staging,prod}`, or takes the art-specific
refetch path. A non-member, unknown id, unavailable artwork or raw-hash request
returns 404 (native) / Subsonic error 70; temporary upstream failures are retryable
errors, not a cross-user fallback. Play/scrobble writes resolve the played track
through the same memberships before accepting the event.

Membership writes are ownership-checked, so membership cannot be self-asserted.
A `library_tracks` row requires an own `track_sources` row, inserted in the
same ingest transaction or already retained. For a local track the owner (via
`releases.local_key` → `ingest_jobs.user_id`) must also be the caller; for a
mirror-verified global track the own source proves the caller's import. Only
ingest `write` creates sources, never a library edit. A `playlist_tracks` row requires all three: the caller owns the
playlist, the track is in at least one of the caller's libraries (library
membership, the same predicate reads use), AND the caller holds an own
`track_sources` row for it (source predicate). Library edits move or copy only
tracks between the caller's own libraries; they cannot mint sources, grants, or
cross-user ownership. `playlist_tracks.track_id` is therefore a doubly-checked
FK, not an unrestricted FK.

Playlist entries have their own ids so the same track may occur repeatedly.
The deferrable `(playlist_id, position)` constraint allows transactional position
shifts/reorders. Removal semantics: deleting a library, a playlist, or an
account runs the explicit batched procedure in rule 3 (enumerate affected
`(user_id, track_id)` pairs and release ids, fixed serializer order, per-batch
membership delete plus both-counts-zero source cleanup and art invalidation) —
never a bare FK cascade that silently drops `library_tracks`/`playlist_tracks`
rows without running that cleanup. Track-row deletion is allowed only after explicit local-catalog GC or consolidation has checked all library/playlist references and remapped references where applicable; it never cascades silently. Removing a track from the caller's last library, or
deleting the last library holding it, does NOT delete that caller's playlist
entries for it: the rows are retained with stable positions and counts, but
become dormant — reads return only the entry id and position with a
`dormant: track not in any library` tombstone instead of track metadata (the
tombstone exposes no track id; none is needed for the restore below), and
`stream`/`scrobble` for a dormant entry return 404 (native) or Subsonic error
70. The retained own `track_sources` row (rule 3) permits restoring that same
local or verified track to an own library **only** through the entry-keyed
restore operation `POST /v1/playlists/{playlist_id}/entries/{entry_id}/restore`
with body `{library_id}` (declared in ADR 0006): it resolves the playlist and
entry ownership first, then the track through the entry and the caller's own
`track_sources` row — never through a caller-supplied catalog id — inserts the
library membership, and re-activates matching dormant entries in place without
reordering. A plain `POST /v1/library` with a non-member track id cannot perform
this restore and stays 404/70. Deleted media/grants are rebuilt by a full own-provider fetch before
streaming. If neither library nor playlist references remained and the source was
deleted, neither the restore operation nor a library edit can restore playable content: require a new ingest.
Ownership of a local id alone must not create a source-less membership; the
restore and any track re-add also require the retained own source. Dormancy does not bypass caps.

Implementation acceptance covers concurrent cross-user and same-user/different-job
imports of one matched release position (one global row, one library count), with
the newer accepted same-user job replacing the source and invalidating only its
stale per-user media/grant mapping; serving selects only a current-source hash and
uses the documented deterministic tie-break. Include opposite-order multi-track
inputs to prove sorted upserts and bounded retries. Cover the same recording on
two releases and two positions on one release (distinct tracks), multidisc positions,
NULL mirror `track_no` remaining unknown/local, unmatched-job retry, deterministic
artist/release grouping across a multi-album manifest retry, and an ambiguous match
retaining its `release_mbid_hint` without guessing. Also require:

- Interleaved PAR2/NFO/audio and archive members produce audio-only ordinals;
  one artist across two release groups preserves those relationships on checkpoint
  replay. Missing pairs or contradictory pairs among distinct occurrences trigger
  the frozen whole-group ordinal fallback, while valid multidisc pairs are
  preserved locally; claimed-position duplicates are collapsed first and never
  trigger the whole-group fallback by themselves.
- Without the producer or mirror occurrence projection, no normalized write mints
  a global release/track, even with plausible manifest MBIDs and positions.
- Tenant A claims a popular release position with a wrong recording/title/duration;
  A's conflicting claim stays local and cannot seed shared metadata. Tenant B's
  correct import still completes with mirror-supplied metadata. An already-existing
  contradictory shared MBID also yields a local fallback with membership/source
  intact, not an import failure or shared overwrite.
- Library-scoped reads and ownership-checked writes: user B requests user A's
  local artist, release and track ids on every catalog endpoint (including
  playlist reads, cover art, plays/scrobble, and directory/list/index calls) and
  gets 404 (native) or Subsonic error 70, and `search3`/search never returns
  them; only rows reachable through the caller's own `library_tracks`
  memberships are readable. B then adds A's local track id to B's library and to
  a B playlist, and every read endpoint — including the playlist, its entries,
  cover art for A's release, and a scrobble for A's track — still returns 404 /
  error 70.
- Same-job duplicates: two files in one NZB (for example FLAC and MP3 copies
  claiming one `(disc, track)` with agreeing titles/durations) resolving to one
  occurrence complete with one track and one source (winner is the lowest
  `audio_ordinal`; loser checkpointed as `duplicate_of` and listed in the job
  result); no statement carries a repeated key and no 21000 occurs. A local
  12-track album plus 12 duplicate-format copies with identical tags yields 12
  tracks with the claimed disc and track numbers preserved. With track tags 1–12
  and **no disc tag on any file**, 12 FLAC + 12 MP3 copies still yield 12 tracks
  at effective disc 1 / claimed tracks 1–12, not 24 ordinal tracks.
- Distinct identities sharing a claimed position are never dropped: 12 singles
  sharing one album title with distinct artists and every file tagged track 1/1
  import as 12 tracks (identity disagreement forces the whole-group
  `(1, audio_ordinal)` fallback); two entries claiming one position but resolved
  by the mirror to different occurrences both write. In a **mixed tagged/untagged
  disc group**, an absent disc remains missing and triggers the local whole-group
  fallback. A malformed/zero disc does not qualify as the all-tags-absent case;
  missing/invalid track tags still trigger fallback. Mirror NULL positions remain
  unknown regardless of the local effective-disc rule.
- Resolved-key artist/release collapse: two text groups in one job
  (`The Beatles`/`Beatles, The` artists, `Abbey Road`/`Abbey Road (Remastered)`
  releases) resolving to the same verified mirror artist and release complete
  with one artist row and one release row; no statement carries a repeated
  `mbid` or `local_key` and no 21000 occurs.
- Text grouping and candidate sets: text-only album (12 tracks with album/artist
  text but no MBIDs) forms one local artist and one local release. An 11-track
  album plus one `feat.` track without TPE2 (two distinct track-artist texts,
  dominant stripped artist >= 2/3) forms one release under the dominant artist,
  not 12 releases. A two-artist split without TPE2 and no dominant artist forms
  one release under the Various-Artists holder. A two-album NZB without TPE2
  (album X by A, album Y by B) forms two releases. A compilation sharing one
  release MBID hangs under the Various-Artists holder with per-track credits
  deferred to manifest tags. Disc is never a group key: a two-disc text-only
  album (disc 1/disc 2 tags, one title) yields one release with `medium_no` 1
  and 2. Twenty-four files with one release MBID and titles `X [CD1]` / `X [CD2]`
  form **one** release with media 1 and 2, even before the mirror projection ships;
  a shared release MBID with no album titles also forms one local release. Distinct
  MBIDs with equal titles stay separate, and no-MBID title-only groups still work.
- Playlist/library consistency: for both a local and a mirror-verified track T,
  remove T from its last library while P references it. Assert stable P entry ids,
  positions and counts, tombstones/no metadata and stream/scrobble 404/70; the own
  source survives but audio mappings/unneeded grants do not. Restore via the public
  entry-keyed operation `POST /v1/playlists/{playlist_id}/entries/{entry_id}/restore`
  with an own `{library_id}` and assert entries re-activate in place and
  own-provider fetch makes streaming work; a plain `POST /v1/library` with the
  dormant track id stays 404/70 and does not restore. Negative cases: restoring
  with another user's entry id, or a dormant entry whose source was deleted,
  returns 404/70. Deleting the last playlist entry (or playlist) with no library membership
  deletes the source; subsequent library-only re-add and the restore operation are
  rejected for both track types. Run a plain `DELETE` of a playlist holding a
  dormant track with no library membership and a plain `DELETE` of a library
  holding T with no playlist reference, and assert the orphaned source is removed
  (including via the GC reconciliation step after a crash between batches) and the
  re-add/restore is rejected; repeat with another reference present and assert the
  source survives. A non-final membership removal retains the source. Race re-add against
  final-reference removal: no committed membership may lack its source. A playlist
  write for a track in no caller library is rejected. Schema FKs on
  `library_tracks`/`playlist_tracks` are `RESTRICT`/non-cascading so a bare
  cascade cannot bypass the procedure. Deleting a library removes that user's
  media for now-unreachable tracks and grants no other current mapping needs.
  Account deletion erases the user's outbox/events/ledger before tracks/users
  (including the `play_events` and outbox track/user FKs), then processes grants,
  media/sources, memberships, local catalog, jobs and remaining user-FK children
  in valid FK order; no local tracks/releases/art or tenant tags survive. Include
  an account with played local tracks and pending outbox rows and assert no 23503
  on track/user deletion. Billing gate: delete a paying and a past_due account and
  assert remote cancellation is confirmed terminal before local billing rows are
  removed; include an account whose `billing_operations` row is still
  `submitted`/`needs_reconcile` at deletion time and assert the gate resolves
  it, lists all remote subscriptions, and cancels each immediately under the
  pinned contract before deleting rows. A `deleted_billing_tombstones` row
  retains customer id (when non-NULL), email HMAC, trial start and open
  invoice/debt refs (no row when no customer and no history); the Stripe
  customer is retained; a webhook for that tombstoned customer with a still-live
  remote subscription cancels it and alerts, otherwise it reconciles without
  recreating a user; and re-registering the same email HMAC is not
  trial-eligible per the ADR 0005 tombstone check. Deleting an account that
  never reached Stripe writes no tombstone and does not wedge. Include a user who
  previously removed the final local library membership and playlist reference:
  owner-keyed GC must find/delete those rows via indexed `local_job_id`, not
  memberships and never by parsing `local_key`. GC played-track case: play a
  local track, remove its final membership and playlist reference, then run the
  owner-keyed GC and assert its outbox/event rows and track delete with no 23503
  while the user still exists; retire a duplicate local track and assert its
  outbox/events remap to the survivor. Race each writer (ingest write,
  library/playlist edit and restore, stream media/grant publication including
  play-event writes, art descriptor/pointer publication) against setting the
  deleting marker; the marker waits for in-flight writers and no writer commits
  after it, and each fenced writer returns 409 with `account_deleting` or
  `container_deleting` plus the blocking operation id. Assert schema marker
  columns and durable `delete_operations` cursor exist, cursor+batch commit
  atomically, crash resumes via the `delete` Workflow or hourly `delete-sweep`,
  re-enumeration includes memberships, grants and media until empty before
  provider/job removal, `DELETE /v1/account|/libraries/{id}|/playlists/{id}`
  return 202 with an operation id readable via `GET /v1/operations/{id}`, and
  account completion removes all of the user's operation rows including
  in-flight scoped ones; a repeat DELETE returns the existing operation id with
  `202`; `GET /v1/operations/{id}` is owner-scoped and reports
  scheduled/pending/running/failed from the stored columns, with `erase_after`
  in the body for account (`scheduled`) rows so the client can show when
  erasure starts and whether cancel is still possible; a fresh account deletion
  polls as `scheduled` with the 24-hour deadline. Library/playlist `done` rows
  persist 7 days; the account result arrives by confirmation email. Index case:
  assert `(user_id, played_at)` and `(track_id)` indexes exist on
  `play_event_outbox` and on partitioned `play_events`, plus `(track_id)`
  indexes on `library_tracks`, `playlist_tracks`, `track_sources` and
  `track_media`, `(source_job_id)` indexes on `track_sources`/`track_media`,
  `(content_hash)` indexes on `track_media`/`user_cache_grants` and partial
  `release_art`, a `(user_id)` index on `auth_challenges` (`audit_log`
  reuses the existing `idx_audit_user` — no duplicate), a partial
  `(action, at)` index on `audit_log` for the `account_deleted:%` retention
  sweep, and a `(pepper_version, email_hmac)` index on
  `deleted_billing_tombstones`.
  At a seeded size assert EXPLAIN uses the index (no Seq Scan, no
  per-partition Seq Scan) for the GC reference test, a 500-track delete batch,
  an ingest_jobs delete during account erasure, the sweep step-5 mark test,
  and the tombstone trial check under every retained pepper version; the delete
  cursor keys on those real paths and the local sweep queries `local_job_id`.
  Tombstone keyring case: rows under v1 and rotated v2 both match, and retiring
  v1 is refused while a v1 row remains.
  FKs remain restrictive; the two rule-5 exceptions are the only play-event
  deletions. Delete-account auth case: without a verified unconsumed
  `step_up_proofs` row the request is `403 step_up_required`; a consumed proof
  replays as `403 step_up_consumed`; a proof for another purpose, session, or
  action, or past its 15-minute `expires_at`, never satisfies the atomic
  consume (consume key is always the proof id from begin). Step-up slot case:
  re-begin after expiry succeeds (prior live row retired in the same
  transaction), switching method from magic-link to passkey-uv succeeds, and
  a 6th begin inside one hour for the same (session, action) is rate-limited;
  a 6th complete attempt caps `attempts` at 5 and requires re-begin. Binding
  case: presenting an ordinary login magic link (no bound `token_hash`) or a
  login WebAuthn challenge (no bound `challenge_id`) to
  `step-up/complete` is rejected, and claiming a step-up link through the
  ADR 0008 login path never verifies a proof and never creates a session.
  With it the request returns `202`, sends the confirmation email,
  creates a `scheduled` row with `erase_after=request+24h`, a sweep at +1h
  does not start erasure, cancel at the boundary versus executor start yields
  exactly one winner (loser 409/too-late), a new-device sign-in during grace
  plus fresh step-up cancels successfully, a login attempt once the operation
  is `running` fails `409 account_deleting` and leaves no real-user
  `audit_log`/`auth_challenges` row, and completion leaves no
  `auth_challenges` row and only the retained sentinel
  (`user_id='deleted'`, `action='account_deleted:<operation_id>'`) audit row
  for that user, run against the real migration schema so the `NOT NULL`
  columns never see NULL (ADR 0006). Billing-gate case: the pinned
  delete-cancel request is sent to stripe-mock with OpenAPI validation and
  accepted (no `cancellation_details[reason]`).
- Cover art: B requests A's local art by `coverart:{release_id}` or raw hash and
  receives 404/70 **before any fetch**. A's member request refills embedded art from
  an own current source; a verified-release member refills CAA art without a
  provider id. Neither creates an audio grant nor changes the cache-bytes gauge.
  Expire the art pointer after 30 idle days, then sweep: no FK failure, descriptor
  retained, authorized refetch works. A missing R2 object uses the same refill.
  Test post-write retry, crashes before/after pointer publication, two authorized
  concurrent refills, GC racing publication, source replacement during embedded
  fetch, and membership removal before publication/serve. No stale source, missing
  byte object or another user's local art may be served; unavailable art is 404/70.
  Assert the post-write stage acts as the job owner, not a system principal;
  repeated negative hits do no upstream work before `retry_after`, and a source
  change clears that wait including `transient_failures`. Three consecutive
  30-second transient timeouts escalate to `unavailable` with the one-hour
  negative cache (counter `transient_failures` 1→2→3 under the release
  serializer); a success or a selected-source change in between resets it to 0.
  Fully removing a non-selected member track with no playlist reference leaves
  the art pointer intact, while removing the selected art-source track clears
  it. A 12-track album embedding the same 3 MiB cover in every file stages one
  R2 object by hash and checkpoints only the hash tuple per entry — the
  checkpoint stays kilobytes, never tens of megabytes. Final-reference cleanup
  invalidates local art in the same transaction only when the selection
  changed, and an expired refill lease cannot publish.

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
- **Ingest manifest producer (new alignment work):** the current parser's flat
  `ParsedNzb.message_ids` and `LibraryWrite.recording_mbids` arrays do not contain
  the per-file selection, grouping or occurrence data above. Preserve NZB file
  boundaries and ordered segments through parse/repair; implement the
  tag/fingerprint producer at the start of `match` / `Matching` before occurrence
  lookup, persist `accepted_audio_manifest_v1` in `JobCheckpoint`, and extend
  `JobCheckpoint` and `LibraryWrite` to carry per-entry selections and match
  classifications into `write` / `Writing`. Do not infer
  file associations by zipping the two legacy flat arrays. Until this producer
  ships, every catalog association remains local; existing legacy writes are not
  evidence that the target manifest or normalized writer already exists.
- **Artwork (new alignment work):** add the manifest's embedded-art selector,
  durable `release_art` descriptor/cache fields, the post-write `art` stage and
  authenticated refill, source-change invalidation, negative caching and the
  three-reference-set GC contract in ADR 0003. Existing `write` remains SQL-only;
  no current Worker is claimed to implement this lifecycle.
- **MusicBrainz mirror (merged):** the current `mb_recording` shape cannot provide
  the per-release medium/track occurrence required by this catalog key. Revise
  ADR 0007 and add a forward-only mirror migration for a tracklist relation keyed
  by `(release_mbid, medium_no, track_no)`, with `recording_mbid` as its reference;
  expose that relation and authoritative MB artist/release/track metadata through
  `workers/mb`. Until that projection ships, **all** normalized release/track writes
  stay local with `release_mbid_hint`, even if the manifest claims exact positions.
  Manifest positions are not a substitute for the mirror. These are implementation
  follow-ups, not changes to the merged mirror or pipeline in this docs-only PR.
- **Stream/Subsonic/billing:** implement the target writers, envelope/pepper
  constraints, grant checks, budgets and lifecycle with their contract tests.
  These changes are not implied by merging this document. Existing in-flight
  work may continue, but cannot claim alignment until its implementation matches.
- **Migration/release:** explicit DB targets, staging upgrade from the existing
  auth schema, replay/constraint checks, and rollback/read compatibility precede
  release. No production migration, data removal or key rotation is done here.
