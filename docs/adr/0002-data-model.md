# ADR 0002: Data Model + ERD (PlanetScale Postgres via Hyperdrive)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Non-blocking note:** Sibling cards (auth, ingest, streaming) already build on
  these table assumptions; this ADR confirms or adjusts them without gating.

## Context

ALL relational data lives in PlanetScale Postgres accessed via Cloudflare
Hyperdrive. No D1. The model must cover accounts, BYOP credentials, library,
play history at scale, and a content-addressed cache index.

## Decision

### Tables

```text
users            id (uuid pk), email (unique), created_at, plan_id (fk → plans)
sessions         id (uuid pk), user_id (fk), created_at, expires_at, revoked_at
plans            id (text pk, e.g. 'free','plus','pro-5x','pro-20x'),
                 price_cents, stream_hours_month, max_concurrent_streams,
                 max_library_tracks, ingest_jobs_day, cache_priority
subscriptions    id (uuid pk), user_id (fk), plan_id (fk), stripe_customer_id,
                 stripe_subscription_id, status, current_period_end
usage_ledger     id (bigserial pk), user_id (fk), period (date),
                 stream_seconds, ingest_jobs, cache_bytes,
                 UNIQUE (user_id, period)
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
library_tracks   library_id (fk), track_id (fk), added_at,
                 pk (library_id, track_id)
playlists        id (uuid pk), user_id (fk), name, created_at
playlist_tracks  playlist_id (fk), track_id (fk), position,
                 pk (playlist_id, track_id)
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
users ──< subscriptions >── plans
users ──1< usage_ledger (per period)
users ──< providers
users ──< indexers (nullable: global indexers have NULL user_id)
users ──< libraries ──< library_tracks >── tracks ──< releases ──< artists
users ──< playlists ──< playlist_tracks >── tracks
users ──< play_events >── tracks
tracks ──< cache_objects (via content_hash join, not FK)
```

### Key rules

1. **BYOP creds are envelope-encrypted** (`providers.encrypted_creds`,
   `indexers.api_key_encrypted`): a per-row DEK encrypted by a KEK held outside
   the DB (Cloudflare Secrets / provider vault). Plaintext creds never touch
   Postgres logs, dumps, or error messages. See the threat-model card.
2. **`play_events` is append-only and monthly-partitioned.** Retention/drop is
   by whole partition. Billing/metering aggregates roll up into `usage_ledger`.
3. **`cache_objects` is the shared dedupe index** (ADR 0003); rows are keyed by
   content hash, never by user.
4. **MusicBrainz linkage is nullable** (`mbid` nullable + indexed): unmatched
   imports work from day one; the MB mirror enriches them later.
5. Migrations live in `migrations/` (forward-only, `migrations/NNNN_*.sql`).

## Consequences

- Hyperdrive connection pooling is mandatory; no direct Postgres connections
  from Workers.
- Partition creation for `play_events` is automated (monthly job); missing
  future partitions must page, not silently drop writes.
- Any new table touching auth/secrets/permissions needs the CISO gate.
