# ADR 0006: API Surface (Native REST + Subsonic Subset)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — versioned HMAC lookup, row-bound token envelopes,
  explicit native/compat auth scope, library-scoped catalog reads,
  and implementation acceptance requirements.
  No completed review score or client-compatibility test result is claimed.

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
   artists,releases,tracks,playlists,plays,providers,indexers,search,stream}`
   plus delete entry points `DELETE /v1/account`, `DELETE
   /v1/libraries/{id}`, `DELETE /v1/playlists/{id}`, `POST
   /v1/account/cancel-deletion` and `GET
   /v1/operations/{operation_id}`. Each DELETE marks `deleting_at`, persists a
   `delete_operations` row (ADR 0002 rule 3) and returns `202` with the operation
   id; a repeat DELETE returns the existing operation id with `202`. The status
   endpoint is owner-scoped (caller must own the row) and reports
   `pending`/`running`/`failed`, plus `done` for retained library/playlist rows;
   an erased account's result arrives by confirmation email and its old
   operation id is thereafter `404`. Writes fenced by a marker return `409`
   with `account_deleting` or `container_deleting` plus the operation id. The
   executor is the `delete` Workflow with hourly `delete-sweep` resume, not a
   bare FK cascade. `DELETE /v1/account` additionally requires fresh step-up
   authentication (passkey with user verification, or a magic-link challenge
   issued within the last 15 minutes), writes an `audit_log` row, and sends a
   confirmation email; erasure starts only after a 24-hour cancellable grace
   window with `deleting_at` set, during which `POST
   /v1/account/cancel-deletion` (same step-up) clears the marker and the
   operation. Library/playlist deletes have no grace window.
   Auth at launch otherwise: the existing session cookie. Native REST bearer keys are
   deferred to a separate scoped-credential design, not borrowed from Subsonic.
   Versioned (`/v1`); breaking changes ship `/v2` alongside, never mutate `/v1`.
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
- **Subsonic auth uses per-client app credentials** (`subsonic_credentials`,
  ADR 0002): random, revocable, scoped to the Subsonic surface. Two forms,
  negotiated per credential (`auth_mode`):
  - `apiKey` (default, OpenSubsonic extension): the client sends `apiKey`
    **without** `u`; the server resolves it via `(pepper_version, api_key_hmac)`
    (HMAC-SHA256 keyed by the exact version in the `SUBSONIC_INDEX_PEPPER`
    Worker-secret keyring defined in ADR 0002, composite UNIQUE lookup).
    It computes candidates under at most
    the current and previous pepper; an old-version match is re-HMACed to
    the current version using the presented key, atomically and without
    storing plaintext. ADR 0002 specifies the retirement precondition for
    dormant credentials: stored HMACs cannot be rehashed without the key.
    The server generates 256-bit CSPRNG keys (minimum entropy 128 bits), not
    caller-selected passwords. A request carrying both `u` and `apiKey` is
    error 43; an unknown key is error 44. Plaintext keys are shown once at
    creation and never stored (HMAC-lookup only).
  - Token form (`u` + `t = md5(secret + salt)`, `s = salt`): only for
    credentials that opt in (`auth_mode` includes `token`); `u` is the
    credential's generated `subsonic_username` (non-PII UNIQUE, shown once
    with the secret — ADR 0002 rule 2), and the server looks up by it then
    unwraps exactly one envelope-encrypted secret (reversible, KEK outside
    the DB, shown once at creation). Persist `token_envelope_version`,
    `token_kek_version`, `token_dek_wrapped_nonce`, `token_dek_wrapped`,
    `token_payload_nonce` and `token_payload_format`, alongside
    `token_secret_encrypted`. Authenticated context binds user, credential id
    and field as defined by ADR 0002. Copying a ciphertext
    to another credential must fail authentication. Email is never accepted
    as `u`.
    Reversibility exists **only** for this form, which the OpenSubsonic
    extension page tells servers to phase out.
  Plaintext `p=` (and its hex alias `p=enc:`) is **rejected** — both put
  long-lived secrets in query strings (logs, threat-model §5); legacy
  clients use `t`/`s`. `/rest/*` logging MUST redact `u`, `p`, `t`, `s`
  and `apiKey` before any sink (access logs, error reports, traces).
  `scope='subsonic'` is an allowlist: the credential reaches only the
  `/rest/*` subset — never `/v1`, provider credentials, or ingest.
  Per-credential rate limits apply, and `last_used_at` anomalies (burst
  from a new IP/ASN) alert.
- Catalog access is library-scoped by default (ADR 0002 §6): every read or
  write that takes or returns a catalog id — native `/v1/artists`,
  `/v1/releases`, `/v1/tracks`, `/v1/search`, `/v1/playlists` (including
  embedded track rows), `/v1/plays`, `/v1/library`, and Subsonic `getArtist`,
  `getAlbum`, `getSong`, `search3`, `getPlaylist(s)`, `getCoverArt`, `stream`,
  `scrobble`, plus `getMusicDirectory`/`getAlbumList2`/`getIndexes` and any
  directory/list/index call where it lists catalog rows — resolves ids only
  through the caller's own `library_tracks` memberships. A non-member id
  returns 404 (native) or Subsonic error 70; search returns only member rows.
  Membership and playlist writes are ownership-checked per ADR 0002 §6 and cannot
  self-assert another user's rows: a playlist write needs the same library
  membership reads need plus an own track source, and entries whose track left
  every caller library stay as dormant tombstones (stable positions/counts;
  stream/scrobble 404/70). Retain their encrypted own source while any own playlist
  entry survives; the entry-keyed restore `POST
  /v1/playlists/{playlist_id}/entries/{entry_id}/restore` with body
  `{library_id}` restores membership — it resolves playlist, entry, and library
  ownership first, then the track through the entry plus the caller's own
  `track_sources` row, never through a caller-supplied catalog id — then an
  own-provider fetch recreates evicted audio media/grants (ADR 0002 rules 3 and
  6). A plain library write with a non-member track id stays 404/70 and cannot
  restore; another user's entry id or a source-less dormant entry is 404/70.
  Neither a dormant entry nor its retained source authorizes a read. `getCoverArt` takes only the
  opaque `coverart:{release_id}` id from `getAlbum`/`getSong`/`search3` and checks
  release membership **before** resolving `release_art`, reading bytes or refilling.
  The primary is the sole launch rendition, `original`. Art uses membership plus
  current entitlement (ADR 0002 rule 4 / ADR 0004),
  **no audio grant**: the post-write `art` stage/refill uses embedded selectors from
  the local owner's current member-track sources, or CAA from a verified release
  MBID (ADR 0003, bounded prefix, per-user concurrency cap, negative-cache rules). Expired/missing art pointers or objects use that same bounded
  refill; unavailable art returns 404/70, temporary upstream failure a retryable
  error. Serve privately, never by raw hash or public R2 URL. A new endpoint that
  takes or returns a catalog id is denied until it declares how it applies this gate.
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
  `apiKey`-without-`u`, error 43 on mixed `u`+`apiKey`, credential
  rotation/revocation, and redaction of auth params in emitted logs).
- Deprecating any Subsonic endpoint needs a new ADR.
- **Implementation alignment:** the Subsonic compatibility implementation
  drops `u+p` (including `p=enc:`), adds generated `subsonic_username` for
  `t`/`s`, adopts versioned HMAC lookup and token envelopes, and reads cache
  bytes only with per-user `track_media` + `user_cache_grants` (ADR 0003).
  This is an acceptance contract, not a claim that the code has migrated.
  Already merged changes need additive follow-up migrations; do not edit an
  applied migration or rely on `CREATE TABLE IF NOT EXISTS` to change it.
  Tests include dual-pepper lookup, dormant-key retirement refusal,
  wrong-user/credential/field envelope swaps, auth-mode/scope checks, and
  cache grants. Compatibility credentials cannot authenticate native REST.
