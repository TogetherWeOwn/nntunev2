# ADR 0002: Data Model + ERD (PlanetScale Postgres via Hyperdrive)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Rev 2026-10-13:** addresses Paperclip Review on PR #2 — per-user cache
  grants + track mapping (findings 1, 3), Subsonic app-password table
  (finding 2), duplicate-safe playlist keys (finding 4), single entitlement
  source (finding 6), split app/mirror migration sequences (finding 8).
- **Rev 2026-10-14 (review round 3):** hash-only `api_key_hash` + `token`
  opt-in for Subsonic creds; grant `last_served_at` lifecycle; deferrable
  playlist position; Stripe-native trial + explicit `active` + partial unique;
  `ingest_jobs` metering writer + daily ledger rows; per-env DB names.
- **Rev 2026-10-14 (review round 4, 4/5 → 5/5):** `api_key_hmac`
  (HMAC-SHA256 with `SUBSONIC_INDEX_PEPPER`, ≥128-bit CSPRNG keys) adopts the
  Subsonic sibling's strictly-stronger column instead of bare SHA-256;
  per-credential non-PII `subsonic_username` lookup for the `t`/`s` form
  (email never accepted as `u`); `track_media.last_served_at` + new
  `track_sources` refetch record (ingest writer); entitlement from
  `trialing`/`active` or `past_due`-inside-`grace_until`, `canceled` terminal
  with `cancel_at_period_end`/`trial_end` columns, provisional signup row +
  reconciler upsert, local `expired` frees the partial-unique slot.
- **Non-blocking note:** Sibling cards (auth, ingest, streaming) already build on
  these table assumptions; this ADR confirms or adjusts them without gating.

## Context

ALL relational data lives in PlanetScale Postgres accessed via Cloudflare
Hyperdrive — across the app database (cluster `nntunev2`, per-environment
database: `nntunev2_staging` in staging, prod DB at prod provisioning) and
the shared `nntunev2-mb` mirror cluster (rule 8). No D1. The model must
cover accounts, BYOP credentials, library, play history at scale, and a content-addressed cache
index with per-user serve authorization (ADR 0003).

## Decision

### Tables

```text
users            id (uuid pk), email (unique), created_at
sessions         id (uuid pk), user_id (fk), created_at, expires_at, revoked_at
subsonic_credentials
                 id (uuid pk), user_id (fk), label,
                 subsonic_username (text, UNIQUE, non-PII, generated
                 at creation and shown once — the `u` value for the
                 md5 t/s form; email is never accepted as `u`),
                 api_key_hmac (text, nullable, UNIQUE — HMAC-SHA256 of
                 the apiKey keyed by the `SUBSONIC_INDEX_PEPPER` Worker
                 secret; lookup column for the u-less apiKey form;
                 keys are ≥128-bit CSPRNG (prefer 256), see rule 2),
                 token_secret_encrypted (bytea, nullable:
                 envelope-encrypted, present only for credentials opting
                 into the md5 t/s form, see rule 2),
                 auth_mode (text: 'apikey' | 'token' | 'both',
                 default 'apikey'),
                 scope (text, default 'subsonic'),
                 created_at, last_used_at, revoked_at
plans            id (text pk, e.g. 'free','plus','pro-5x','pro-20x'),
                 price_cents, stream_hours_month, max_concurrent_streams,
                 max_library_tracks, ingest_jobs_day, cache_priority
subscriptions    id (uuid pk), user_id (fk), plan_id (fk), stripe_customer_id,
                 stripe_subscription_id (UNIQUE — reconciler upserts on this),
                 status (text: trialing|active|past_due|canceled|expired —
                 `canceled` is Stripe-terminal, `expired` is local-terminal),
                 current_period_end (timestamptz),
                 cancel_at_period_end (bool, default false — end-of-period
                 cancel stays `active` with this true until Stripe ends it),
                 trial_end (timestamptz, nullable),
                 grace_until (timestamptz, nullable — reconciler-set, 7d from
                 entering past_due; entitlement window, see rule 4),
                 partial UNIQUE (user_id) WHERE status IN
                 ('trialing','active','past_due') — one live row per user;
                 `canceled`/`expired` rows sit outside the predicate so a
                 resubscribe (new stripe_subscription_id) never collides
usage_ledger     id (bigserial pk), user_id (fk), period (date: calendar
                 day — monthly caps sum the days of the month),
                 stream_seconds, ingest_jobs, cache_bytes,
                 UNIQUE (user_id, period)
ingest_jobs      id (uuid pk), user_id (fk), created_at (timestamptz),
                 status (text: queued|running|done|failed)
providers        id (uuid pk), user_id (fk), label,
                 encrypted_creds (bytea: envelope-encrypted, see below),
                 created_at, last_verified_at
indexers         id (uuid pk), user_id (fk, nullable for global),
                 name, base_url, api_key_encrypted (bytea, nullable), enabled
libraries        id (uuid pk), user_id (fk), name, created_at
artists          id (uuid pk), mbid (uuid, nullable, indexed), name
releases         id (uuid pk), mbid (uuid, nullable, indexed),
                 artist_id (fk), title, release_type, year
tracks           id (uuid pk), mbid (uuid, nullable, indexed),
                 release_id (fk), title, track_no, duration_ms
track_media      user_id (fk), track_id (fk),
                 content_hash (fk → cache_objects),
                 format (text), bitrate (int), verified_at,
                 last_served_at (timestamptz, refreshed when this user's
                 track is served — bounds the GC mark half, ADR 0003),
                 pk (user_id, track_id, content_hash)
track_sources    user_id (fk), track_id (fk),
                 indexer_id (fk → indexers, nullable),
                 nzb_ref_encrypted (bytea: envelope-encrypted NZB ref /
                 ordered segment message-id list — the refetch source,
                 written by the ingest worker at accept time, kept for
                 the life of the library track, deleted with it),
                 created_at (timestamptz),
                 pk (user_id, track_id)
user_cache_grants
                 user_id (fk), content_hash (fk → cache_objects),
                 provider_id (fk → providers, nullable),
                 verified_at, last_served_at (timestamptz, refreshed on
                 every serve — bounds the GC mark window, ADR 0003),
                 pk (user_id, content_hash)
library_tracks   library_id (fk), track_id (fk), added_at,
                 pk (library_id, track_id)
playlists        id (uuid pk), user_id (fk), name, created_at
playlist_tracks  id (uuid pk), playlist_id (fk), track_id (fk),
                 position (int),
                 UNIQUE (playlist_id, position)
                 DEFERRABLE INITIALLY DEFERRED — mid-playlist inserts
                 and reorders shift positions in one statement,
                 which a non-deferrable UNIQUE rejects per row,
                 INDEX (playlist_id, track_id)
play_events      user_id (fk), track_id (fk), played_at (timestamptz),
                 stream_seconds, cached_hit (bool)
                 PARTITION BY RANGE (played_at), one partition per month
cache_objects    content_hash (text pk, e.g. sha256 hex),
                 byte_size (bigint), r2_key (text),
                 ref_count (bigint), created_at, last_verified_at
```

### ERD (text)

```text
users ──< sessions
users ──< subsonic_credentials
users ──< subscriptions >── plans
users ──1< usage_ledger (per period)
users ──< providers ──< user_cache_grants >── cache_objects
users ──< user_cache_grants (per-user right to read cached bytes)
users ──< track_media >── tracks (per-user track→bytes mapping)
track_media >── cache_objects (via content_hash FK)
users ──< track_sources >── tracks (per-user refetch source, ingest-written)
users ──< indexers (nullable: global indexers have NULL user_id)
users ──< libraries ──< library_tracks >── tracks ──< releases ──< artists
users ──< playlists ──< playlist_tracks >── tracks
users ──< play_events >── tracks
```

### Key rules

1. **BYOP creds are envelope-encrypted** (`providers.encrypted_creds`,
   `indexers.api_key_encrypted`): a per-row DEK encrypted by a KEK held outside
   the DB (Cloudflare Secrets / provider vault). Plaintext creds never touch
   Postgres logs, dumps, or error messages. See the threat-model card.
2. **Subsonic credentials are HMAC-lookup by default, reversible only for
   the md5 `t`/`s` opt-in:** the default `auth_mode='apikey'` stores only
   `api_key_hmac` = HMAC-SHA256(apiKey, `SUBSONIC_INDEX_PEPPER` Worker
   secret), UNIQUE lookup — the plaintext key is shown once at creation and
   never stored, and requests carry `apiKey` without `u` (ADR 0006). Bare
   unsalted SHA-256 is rejected: a DB dump (backup, replica) would permit
   offline guessing against a fast hash, so this ADR adopts the Subsonic
   sibling's `api_key_hmac` + pepper column (same lookup shape, strictly
   stronger) rather than downgrading it. Keys are ≥128-bit CSPRNG (prefer
   256-bit); the server rejects shorter/procedural keys at creation. Only
   credentials with `auth_mode` including `token` carry
   `token_secret_encrypted` (a per-row DEK ciphertext like rule 1), because
   Subsonic token auth (`t = md5(secret + salt)`) needs the plaintext at
   verify time. Each token-enabled credential gets a generated non-PII
   `subsonic_username` (UNIQUE, shown once with the secret) that **is** the
   `u` value for the token form: the server looks up by it, then unwraps
   exactly one secret — email is never accepted as `u` (PII in query
   strings, plus trial-unwrap-per-credential KEK cost on every
   `stream`/`getCoverArt` call). Secrets are revocable per client and scoped
   to the Subsonic surface. They have no relationship to passkey /
   magic-link credentials, which have no password to map. Table name
   `subsonic_credentials` is canonical; the sibling Subsonic card's
   `subsonic_clients` shape (`api_key_hmac` UNIQUE) reconciles into it
   (the sibling keeps its column, this ADR adopts it).
3. **`cache_objects` is a storage dedupe index, never an authorization.**
   Serving R2 bytes additionally requires a `user_cache_grants` row for
   (user, hash), created only after that user's own provider fetched and
   hash-verified the object — and the track→bytes mapping is per-user
   (`track_media`), so one user's bad ingest can never poison a track for
   everyone. This reconciles threat-model §5 (never serve user A's bytes on
   user B's grant): storage sharing is invisible and unaddressable without
   the grant row. Grant/mapping lifecycle: removing a library track deletes
   its `track_media` + `track_sources` rows; cache eviction deletes the
   evicted grant rows first, then the newly-orphaned `track_media` rows
   (orphaned = a `track_media` row whose `(user_id, content_hash)` has no
   matching `user_cache_grants` row, or whose `track_id` left the user's
   libraries), then sweepable `cache_objects` — the FK
   `track_media.content_hash → cache_objects` requires this order, a stale
   grant alone never makes an object sweepable while a `track_media` row
   still points at it. `last_served_at` refreshes on every serve on **both**
   the grant row and the served `track_media` rows, so both halves of the
   GC mark set are windowed (ADR 0003). Per-user charging for the shared
   store: a user's `cache_bytes` = SUM(`byte_size`) over **that user's**
   grants — a shared object's bytes count toward every granted user's
   budget, and evicting user A's grant frees A's budget even while user B's
   grant keeps the object alive (the object is deleted only at zero grants
   plus zero referencing `track_media` rows). Refetch sources live in
   `track_sources`, written by the ingest worker in the accepting
   transaction and kept for the life of the library track, so eviction is
   non-destructive (ADR 0003). See ADR 0003.
4. **Entitlement has one source of truth:** the effective plan is derived —
   the single live `subscriptions` row's plan, else nothing (hard paywall,
   ADR 0004). Entitled means `status` in (`trialing`, `active`), **or**
   `status = 'past_due'` with `grace_until > now()` (7-day reconciler-set
   grace; ADR 0005). `canceled` is terminal and **never** entitled: an
   end-of-period cancel stays `active` with `cancel_at_period_end = true`
   until Stripe ends it (that is the paid-through case — not a `canceled`
   row with a future period end), and a Stripe-side cancel after dunning
   has already rolled into the unpaid period. The partial unique index
   covers exactly the non-terminal set (`trialing`,`active`,`past_due`),
   so a resubscribe (new `stripe_subscription_id`) never collides with an
   old terminal row and "the single active row" is unambiguous. A `past_due`
   row past grace no longer blocks its replacement: the reconciler moves it
   to local-terminal `expired` (outside the predicate), freeing the slot —
   the predicate cannot express "inside grace", so `grace_until` carries it.
   Trial is Stripe-native only: signup creates a `trialing` subscription on
   the trial plan with `trial_settings.missing_payment_method = cancel`
   (ADR 0005) — there is no separate app-side `users.created_at` clock.
   While `trialing`, trial-plan caps apply (10 stream hrs, ADR 0004), not
   the converted price's caps. The signup handler writes the provisional
   `trialing` row in the same transaction as user creation (so a new signup
   is entitled before `customer.subscription.created` arrives); the Stripe
   webhook reconciler upserts on `stripe_subscription_id` thereafter and is
   the authoritative updater. After the 14-day window with no paid
   subscription Stripe cancels the trial (per the pinned setting) and there
   is no entitlement. The `users` table carries no `plan_id`. Cap
   enforcement (ADR 0004) reads this derived plan only.
5. **`play_events` is append-only and monthly-partitioned.** Retention/drop is
   by whole partition. Billing/metering aggregates roll up into `usage_ledger`
   (daily rows; monthly caps sum the calendar month), which is the metering
   authority (ADR 0004); Analytics Engine is analytics only. `stream_seconds`
   rolls up from `play_events`; `ingest_jobs` counts roll up from the
   `ingest_jobs` table (counted in the same transaction that accepts a job);
   `cache_bytes` rolls up from cache accounting. Concurrent-stream caps are
   enforced from the per-user pool Durable Object, which is the authority
   for live stream count.
6. **`cache_objects` rows are keyed by content hash, never by user** (ADR 0003);
   `track_media` / `user_cache_grants` carry the per-user scope.
7. **MusicBrainz linkage is nullable** (`mbid` nullable + indexed): unmatched
   imports work from day one; the MB mirror enriches them later.
8. **Migrations are split per database** (forward-only): `migrations/app/NNNN_*.sql`
   targets the app database (cluster `nntunev2`, database `nntunev2_staging`
   in staging; the prod database is created at prod provisioning),
   `migrations/mb/NNNN_*.sql` targets the shared `nntunev2-mb` mirror
   cluster — independent sequences, matching the provisioned ops plan and
   ADR 0007 (per-environment names from the ops runbook; `migrations/app`
   is applied to staging first, then prod). (ADR 0007's
   `migrations/0001_mb_mirror.sql` moves to `migrations/mb/`; owned by the MB
   card, non-blocking here.)

## Consequences

- Hyperdrive connection pooling is mandatory; no direct Postgres connections
  from Workers. Each database has its own Hyperdrive binding (app vs mirror).
- The stream path resolves track → bytes via `track_media` +
  `user_cache_grants`, never via a global mapping.
- Partition creation for `play_events` is automated (monthly job); missing
  future partitions must page, not silently drop writes.
- Any new table touching auth/secrets/permissions needs the CISO gate.
- **Known rework this ADR imposes on sibling cards:** the auth/vault card
  migrates to uuid PKs + timestamptz (from TEXT ids / BIGINT epoch) and to
  multi-row `providers` (one row per provider label, envelope-encrypted per
  row) instead of a single `provider_vaults` row per user; the Subsonic card
  adopts `api_key_hmac` + `subsonic_username` (rule 2) instead of bare
  SHA-256 / email-as-`u`; the ingest card writes `ingest_jobs` in the
  accepting transaction and `track_sources` alongside `track_media`
  (rule 5 / rule 3). Owned by the auth, Subsonic compat and ingest cards —
  non-blocking here, but their review must show the alignment.
