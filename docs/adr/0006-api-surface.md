# ADR 0006: API Surface (Native REST + Subsonic Subset)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Rev 2026-10-13:** addresses Paperclip Review on PR #2 — Subsonic auth via
  per-client app passwords + `apiKey` (finding 2, security), spec-valid error
  codes (finding 7). Exact code/auth mapping is pinned by the Subsonic compat
  card's contract tests against real clients.

## Context

Clients need a first-class API, and the existing ecosystem (Symfonium, DSub,
navidrome-compatible clients) speaks Subsonic. Building only one strands
either our own frontend or every third-party client.

Accounts use passkeys + email magic links: there is no password to map
Subsonic `u/p/s/t` auth onto, and plaintext `p=` puts secrets in query
strings (logs, threat-model §5). So the compat surface gets its own
least-privilege credential.

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
   Subsonic error **0** (generic, with a human message; 70 where the resource
   itself is missing) plus an `nntune-native` hint header pointing at the
   REST equivalent. No invented codes — clients map known codes to behaviour,
   and unknown codes degrade to generic.

### Rules

- One auth/session layer serves both; caps in ADR 0004 apply equally — no
  bypass via the compat surface.
- **Subsonic auth uses per-client app passwords** (`subsonic_credentials`,
  ADR 0002): random, revocable, scoped to the Subsonic surface. Clients send
  `u` plus either the token form (`t = md5(app_password + salt)`, `s = salt`,
  preferred) or the OpenSubsonic `apiKey` extension where supported (invalid
  key → error 44). Plaintext `p=` is **rejected**; `p=enc:` is accepted only
  over TLS for legacy clients, never logged, and discouraged in docs.
  Verification needs the plaintext secret, so app secrets are
  envelope-encrypted (reversible, KEK outside the DB), shown once at creation
  — see ADR 0002 rule 2.
- The REST OpenAPI spec is generated from code (Rust types → schema), checked
  into the repo, and CI-diffed: spec drift fails the build.
- Subsonic responses are adapters over the same handlers, not a parallel
  implementation; new features land in REST first, Subsonic only if the
  upstream spec covers them.

## Consequences

- Third-party clients work on day one for playback + library browse; advanced
  features (BYOP management, ingest jobs) are REST-only.
- Fuzz/contract tests cover the Subsonic subset against known client quirks
  (case sensitivity, `c=` client param, token/salt and `apiKey` vectors,
  app-password rotation/revocation).
- Deprecating any Subsonic endpoint needs a new ADR.
