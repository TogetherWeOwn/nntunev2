# workers/mb — MusicBrainz mirror read API

Rust (`workers-rs` → `wasm32`) read API over the `nntunev2-mb` Postgres
database (via Hyperdrive). Schema: `migrations/0001_mb_mirror.sql`.
Decision: `docs/adr/0007-mb-mirror.md`.

## Endpoints

| Method | Path | Notes |
|---|---|---|
| `GET` | `/v1/mb/artist/:mbid` | Artist lookup; stub answers `501` + `no-store` until DB wiring lands, then `Cache-Control: public, max-age=86400` |
| `GET` | `/v1/mb/release/:mbid` | Release lookup (includes `caa_url` when art is cached); same stub caching as artist |
| `GET` | `/v1/mb/recording/:mbid` | Recording lookup; same stub caching as artist |
| `GET` | `/v1/mb/search?q=&type=` | `type` in `artist\|release\|recording`; `q` min 2 chars; trigram (`pg_trgm`) ordered by similarity; `Cache-Control: public, max-age=300` |

Errors are JSON (`{"error": "..."}`); malformed `:mbid` (must be UUID),
unknown `type` and short queries are `400`; the pre-wiring lookup stub is
`501` with `Cache-Control: no-store`; unknown routes are `404`.

## Caching

- Lookups: KV first (`mb:<kind>:<mbid>`), 24 h TTL; DB via Hyperdrive on
  miss; `Cache API` at the edge with the same TTL.
- Search: KV key includes normalized `q` + `type`, 5 min TTL; no negative
  caching beyond the edge TTL.
- Cover art bytes live in R2 `nntunev2-art` (`art/{hh}/{hh}/{sha}-{size}`,
  ADR 0003); this worker returns URLs, never proxies full images inline.

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
