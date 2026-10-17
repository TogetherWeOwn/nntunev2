# workers/mb — MusicBrainz mirror read API

Rust (`workers-rs` → `wasm32`) read API over the `nntunev2-mb` Postgres
database (via Hyperdrive). Schema: `migrations/0001_mb_mirror.sql`.
Decision: `docs/adr/0007-mb-mirror.md`.

## Endpoints

| Method | Path | Notes |
|---|---|---|
| `GET` | `/healthz` | Unauthenticated Worker health check; `{"ok":true}`, `Cache-Control: no-store`; does not access bindings |
| `GET` | `/v1/mb/artist/:mbid` | Artist lookup over Hyperdrive (`MB_DB`); success `Cache-Control: public, max-age=86400` |
| `GET` | `/v1/mb/release/:mbid` | Release lookup (includes the stored `caa_url`); same caching as artist |
| `GET` | `/v1/mb/recording/:mbid` | Recording lookup; same caching as artist |
| `GET` | `/v1/mb/search?q=&type=` | `type` in `artist\|release\|recording`; `q` 2-100 chars; trigram (`pg_trgm`) ordered by similarity, `LIMIT 25`; success `Cache-Control: public, max-age=300` |

Errors are JSON (`{"error": "..."}`) with `Cache-Control: no-store`:
malformed `:mbid` (must be UUID), unknown `type` and short/long queries
are `400`; a well-formed MBID with no row and unknown routes are `404`;
mirror read failures are a generic `500` (no DB detail leaves the
worker). Lookup success is the flat entity fields plus `"kind"`;
search success is `{"q", "type", "results": [{"mbid", "name"|"title",
"score"}]}`; release results also carry `artist_mbid`/`artist_name`
(null when the release has no artist) so callers can verify
"Artist - Album" queries against the artist instead of the title-only
score. Interactive reads never call musicbrainz.org.

## Caching

- Order per read: edge Cache API, then KV, then Hyperdrive Postgres.
- Lookups: KV key `mb:lookup:<kind>:<mbid>` (lowercase MBID), 24 h TTL;
  the same TTL is served as `Cache-Control` and stored via the Cache API.
- Search: KV key `mb:search:<kind>:<normalized q>` (trimmed, lowercased,
  whitespace collapsed; SQL binds the same normalized value), 5 min TTL.
- Only successes are cached (a successful search with zero hits counts).
  400/404/500 responses are `no-store` and written to neither cache.
  KV failures degrade to plain DB reads; a missing `MB_CACHE` binding
  disables KV, a missing/unreachable `MB_DB` yields the generic `500`.
- Release responses currently preserve the stored `caa_url` unchanged, so
  lookup cache keys remain environment-independent. When public R2 hosts are
  activated and URLs become environment-specific, include the environment in
  lookup cache keys before rewriting `caa_url`.
- Cover art bytes are intended for the per-environment R2 art buckets
  (`nntunev2-art-staging` / `nntunev2-art-prod`, `docs/ops.md` inventory;
  keys `art/{hh}/{hh}/{sha}-{size}`, ADR 0003). Public release `caa_url`
  generation remains deferred until bucket bindings and public hosts are
  activated; this worker never proxies full images inline.

## p95 methodology (acceptance input)

Staging measures, per endpoint, over 1k requests (50 concurrent) against
staging Hyperdrive + KV:

- `lookup p95 < 250 ms` (cache miss) / `< 50 ms` (cache hit)
- `search p95 < 600 ms` (trigram, `LIMIT 25`)

Numbers are recorded here once staging exists; this slice defines the
method, not the measurements. Replication SLO: **lag < 2 h**
(`mb_import_state.lag_seconds`).

## Terms

Core MusicBrainz data is CC0; CAA images follow their per-image terms.
Commercial use notes the MetaBrainz support tier (see ADR 0007).

## Dev

```sh
cp wrangler.toml.example wrangler.toml  # fill ids locally, never commit
cargo fmt --check --manifest-path workers/mb/Cargo.toml \
  && cargo clippy --manifest-path workers/mb/Cargo.toml --target wasm32-unknown-unknown -- -D warnings \
  && cargo test --manifest-path workers/mb/Cargo.toml
```

Skills used: executing-plans, verification-before-completion.
