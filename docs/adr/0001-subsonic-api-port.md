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

- Port the v1 surface exactly: 48 supported endpoints, 9 explicit refusals
  (error 0 in a normal HTTP-200 envelope), `hls.m3u8` as plain 410,
  envelope shaped by `SubsonicResponse` (attributes/children/list rules,
  `value`-as-text, bool and null handling), auth order
  `apiKey` → `u+p` (incl. `p=enc:<hex>`) → `u+t+s` with codes
  40/43/44/10/50/70 (43 apiKey mixed with legacy params, 44 invalid key,
  10 token without salt or bad JSONP callback, 50 per-user forbidden).
  Every user-state `Library` method takes the authenticated identity;
  playlist/star/rating/queue rows are per-user, and the worker's
  `MemStore::empty()` accepts no credentials until PlanetScale lands.
- Pure-logic crate `crates/nntune-subsonic` (zero dependencies, native +
  `wasm32`) behind a `Library` trait; thin `workers/api` shell adapts the
  runtime. PlanetScale/R2 backends implement `Library` in a follow-up —
  until then the worker serves an empty catalogue, never fabricated rows.
- Golden XML/JSON fixtures under `fixtures/subsonic/` capture v1 behaviour;
  the Rust fixture suite asserts byte equality, and a dependency-free Python
  script validates fixture shape in CI.
- Two deliberate deviations from v1, both client-invisible in practice:
  envelope `type` is `nntune` (v1 inherited `koel`); per-client API keys
  (`subsonic_clients`: HMAC-peppered lookup index plus sealed legacy
  secret for `legacy_enabled` clients, with revocation) replace the
  single per-user key so keys revoke per device.

## Consequences

- Clients keep working unchanged; `getOpenSubsonicExtensions` still claims
  only `songLyrics` (no transcoding exists to negotiate, as in v1).
- `maxBitRate`/`timeOffset` stay accepted-and-ignored, matching v1.
- Follow-ups: PlanetScale `Library` impl, stream-worker byte wiring,
  staging smoke per `docs/subsonic-client-smoke.md`.
