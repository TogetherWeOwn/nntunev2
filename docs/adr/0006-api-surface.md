# ADR 0006: API Surface (Native REST + Subsonic Subset)

- **Status:** Accepted
- **Date:** 2026-10-13

## Context

Clients need a first-class API, and the existing ecosystem (Symfonium, DSub,
navidrome-compatible clients) speaks Subsonic. Building only one strands
either our own frontend or every third-party client.

## Decision

**Two surfaces on one core:**

1. **Native REST (OpenAPI 3.1, `openapi.yaml` at repo root of `workers/api`):**
   the canonical API. `apps/web` uses only this. Resources: `/v1/{me,library,
   artists,releases,tracks,playlists,plays,providers,indexers,search,stream}`.
   Auth: session cookie (web) + bearer API keys (clients). Versioned (`/v1`);
   breaking changes ship `/v2` alongside, never by mutating `/v1`.
2. **Subsonic subset** (`/rest/*`, OpenSubsonic-aligned): `ping`, `getLicense`,
   `getMusicFolders`, `getIndexes`, `getMusicDirectory`, `getAlbumList2`,
   `search3`, `getArtist`, `getAlbum`, `getSong`, `stream`, `getCoverArt`,
   `getPlaylist(s)`, `scrobble`. Anything outside the subset returns
   Subsonic error 78 ("not implemented") with an `nntune-native` hint header
   pointing at the REST equivalent.

### Rules

- One auth/session layer serves both; Subsonic `u`/`p`/`s`/`t` auth maps onto
  the same session + plan enforcement as REST (caps in ADR 0004 apply
  equally — no bypass via the compat surface).
- The REST OpenAPI spec is generated from code (Rust types → schema), checked
  into the repo, and CI-diffed: spec drift fails the build.
- Subsonic responses are adapters over the same handlers, not a parallel
  implementation; new features land in REST first, Subsonic only if the
  upstream spec covers them.

## Consequences

- Third-party clients work on day one for playback + library browse; advanced
  features (BYOP management, ingest jobs) are REST-only.
- Fuzz/contract tests cover the Subsonic subset against known client quirks
  (case sensitivity, `c=` client param, hex vs. raw password forms).
- Deprecating any Subsonic endpoint needs a new ADR.
