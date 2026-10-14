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
                 api_key_hash (text, nullable, UNIQUE — SHA-256 of the
                 apiKey; lookup column for the u-less apiKey form),
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
                 stripe_subscription_id, status, current_period_end,
                 partial UNIQUE (user_id) WHERE status IN
                 ('trialing','active','past_due') — one live row per user
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
                 pk (user_id, track_id, content_hash)
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
2. **Subsonic credentials are hash-only by default, reversible only for the
   md5 `t`/`s` opt-in:** the default `auth_mode='apikey'` stores only
   `api_key_hash` (SHA-256, UNIQUE lookup) — the plaintext key is shown once
   at creation and never stored, and requests carry `apiKey` without `u`
   (ADR 0006). Only credentials with `auth_mode` including `token` carry
   `token_secret_encrypted` (a per-row DEK ciphertext like rule 1), because
   Subsonic token auth (`t = md5(secret + salt)`) needs the plaintext at
   verify time. Secrets are random, shown once, revocable per client, and
   scoped to the Subsonic surface. They have no relationship to passkey /
   magic-link credentials, which have no password to map. Table name
   `subsonic_credentials` is canonical; the sibling Subsonic card's
   `subsonic_clients` shape (`api_key_hash` UNIQUE) reconciles into it.
3. **`cache_objects` is a storage dedupe index, never an authorization.**
   Serving R2 bytes additionally requires a `user_cache_grants` row for
   (user, hash), created only after that user's own provider fetched and
   hash-verified the object — and the track→bytes mapping is per-user
   (`track_media`), so one user's bad ingest can never poison a track for
   everyone. This reconciles threat-model §5 (never serve user A's bytes on
   user B's grant): storage sharing is invisible and unaddressable without
   the grant row. Grant/mapping lifecycle: removing a library track deletes
   its `track_media` rows; cache eviction deletes the evicted grant rows
   (ADR 0003); `last_served_at` refreshes on every serve so the GC mark set
   is bounded and eviction actually reclaims. See ADR 0003.
4. **Entitlement has one source of truth:** the effective plan is derived —
   the single active `subscriptions` row's plan, else nothing (hard paywall,
   ADR 0004). `active` means `status` in (`trialing`, `active`), plus
   `past_due` inside grace and `canceled` with `current_period_end` in the
   future; a partial unique index permits one such row per user. Trial is
   Stripe-native only: a `trialing` subscription on the trial plan created
   at signup — there is no separate app-side `users.created_at` clock, so
   the two trial mechanisms of the previous revision collapse into one.
   After the 14-day window with no paid subscription there is no
   entitlement. The `users` table carries no `plan_id`; the Stripe webhook
   reconciler is the sole writer of `subscriptions`. Cap enforcement
   (ADR 0004) reads this derived plan only.
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
