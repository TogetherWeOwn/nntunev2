# ADR 0001: Subsonic-compatible API port (Rust)

## Status

Accepted (slice 1: envelope + auth + routing + in-memory catalogue).

## Context

NNTune v1 exposes ~50 Subsonic endpoints (`routes/subsonic.php`) that DSub,
Symfonium and Feishin depend on. The v2 rewrite must keep that surface or
existing clients break. Stack decision (final): Rust `workers-rs` → `wasm32`
for all backend workers, PlanetScale Postgres via Hyperdrive for relational
data, R2 for bytes.

## Decision

- Port the v1 surface exactly: 49 supported endpoints, 9 explicit refusals
  (error 0 in a normal HTTP-200 envelope), `hls.m3u8` as plain 410,
  envelope shaped by `SubsonicResponse` (attributes/children/list rules,
  `value`-as-text, bool and null handling), auth order
  `apiKey` → `u+p` (incl. `p=enc:<hex>`) → `u+t+s` with codes
  40/43/44/10/50/70 (43 apiKey mixed with legacy params, 44 invalid key,
  10 token without salt, oversized POST body or bad JSONP callback,
  50 `User is not authorized for the given operation.`,
  70 `The requested data was not found.`).
  Every user-state `Library` method takes the authenticated identity;
  playlist/star/rating/queue rows are per-user, `getUser`/`getAvatar`
  enforce self-or-admin, and the worker's `MemStore::empty()` accepts no
  credentials until PlanetScale lands.
- `getOpenSubsonicExtensions` is public (outside auth, per the OpenSubsonic
  spec) claiming exactly `apiKeyAuthentication` v1 + `formPost` v1;
  `tokenInfo` reports the caller's username; POST bodies over 10 MB are
  refused before routing/auth; JSONP uses Symfony's `/**/{cb}({...});`
  framing, `text/javascript` type and `setCallback` validator;
  `getUser`/`SongResource` shapes and the 50/70 texts match v1 source.
- Pure-logic crate `crates/nntune-subsonic` (zero dependencies, native +
  `wasm32`) behind a `Library` trait; thin `workers/api` shell adapts the
  runtime. PlanetScale/R2 backends implement `Library` in a follow-up —
  until then the worker serves an empty catalogue, never fabricated rows.
- Golden XML/JSON fixtures under `fixtures/subsonic/` pin the v1-derived
  contract (shapes verified field-by-field against v1 source; no live v1
  capture exists yet — see the fixtures README); the Rust fixture suite
  asserts byte equality, and a dependency-free Python script validates
  fixture shape in CI. The root workspace and standalone API commit the
  lockfiles used for verification; the standalone API jobs and the lockfile-freshness check
  use `--locked`, the root workspace test and clippy jobs do not.
- Two deliberate deviations from v1, both client-invisible in practice:
  envelope `type` is `nntune` (v1 inherited `koel`); per-client API keys
  (`subsonic_clients`: HMAC-peppered lookup index plus sealed legacy
  secret for `legacy_enabled` clients, with revocation) replace the
  single per-user key so keys revoke per device.

## Compatibility details

- The two album-list routes have different v1 backends. Legacy `getAlbumList`
  supports all ten validated types: frequent/recent use authenticated-user
  play history, reversed year bounds request descending order, and random
  ignores offset. Catalogue `getAlbumList2` supports newest, random, byYear,
  alphabeticalByName and alphabeticalByArtist; the other validated types
  return error 0. Its reversed year bounds return an empty list, and its year
  ordering uses album release date, then catalogue curation time (not legacy
  row creation); nullable descending dates follow PostgreSQL NULLS FIRST.
  Catalogue artist ordering uses its sort key rather than the display name.
  Album years are not inferred from tracks. Favorites follow position order
  across both starred routes and the legacy starred album list; re-starring an
  existing item moves it to the end without duplicating its membership.
- `Library::album_list`, `album_list2`, `random_songs` and
  `artists_last_modified` are required store methods. Production stores must
  implement visibility, real sampling, user history and catalogue timestamps;
  there are no fallback rankings based on IDs or track counts. `MemStore`
  keeps the fixture clock and a seedable, advancing shuffle sequence for
  reproducible multi-record catalogues; its RNG is not for secrets.
- Scrobbles with `submission=false` are now-playing notifications, not plays.
  Structured lyrics use `lang=und`, preserve offset separately, remove known
  metadata tags and sort trimmed timed lines. Plain lyrics have no `start`;
  metadata-only or offset-only input returns an empty lyrics list.
  Artist search omits unservable artwork; index buckets use v1's article and
  uppercase-then-ASCII rules, including multi-character uppercase initials.
- A form POST with no runtime body still dispatches its query parameters.
  A real body read or UTF-8 failure returns an HTTP-200 error-10 envelope
  without dispatching partial parameters. The declared and streamed 10 MiB
  caps remain in place; only the query selects the body-error envelope format.

## Consequences

- Clients keep working unchanged; `getOpenSubsonicExtensions` claims
  exactly what v1 claims (`apiKeyAuthentication` + `formPost`);
  `songLyrics` stays unclaimed because `getLyrics` is a stub, as in v1.
- `maxBitRate`/`timeOffset` stay accepted-and-ignored, matching v1.
- Follow-ups: PlanetScale `Library` impl, stream-worker byte wiring,
  live v1 fixture capture, staging smoke per
  `docs/subsonic-client-smoke.md`.
