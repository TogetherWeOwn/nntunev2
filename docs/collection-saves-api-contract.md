# Collection-saves API contract (durable saved albums + followed artists)

This is the binding wire contract for the per-account saved-album and
followed-artist slice served by the API worker. Integrators consume exactly
these routes and fields; anything not listed here is not part of the
contract. The slice is an account preference only: it never ingests media,
grants access, reports listens, or touches billing. The owning issue's
parent consumes this contract; it has no other consumer.

## Routes

All routes live under the native `/v1` surface and share the worker's
request-body cap for empty-bodied methods (every route below is bodiless).

| Method and path | Purpose |
| --- | --- |
| `PUT /v1/me/library/albums/{mbid}` | Save a release MBID for the caller. Idempotent replay: repeats return the original `saved_at`, never a second row. |
| `DELETE /v1/me/library/albums/{mbid}` | Remove the save. Idempotent: removing an unsaved or already-removed MBID still succeeds. Removal stays reachable for targets whose membership later lapsed — it does not re-run the target validation below. |
| `GET /v1/me/library/albums/{mbid}` | Saved-state read for one release MBID: `{"saved": bool, "saved_at": string-or-null}`. |
| `GET /v1/me/library/albums` | Bounded first page of the caller's saved albums, newest save first. |
| `PUT /v1/me/library/artists/{mbid}` | Follow an artist MBID. Same idempotent-replay rule. |
| `DELETE /v1/me/library/artists/{mbid}` | Remove the follow. Same idempotent-removal rule. |
| `GET /v1/me/library/artists/{mbid}` | Follow-state read: `{"followed": bool, "followed_at": string-or-null}`. |
| `GET /v1/me/library/artists` | Bounded first page of followed artists, newest follow first. |

`{mbid}` is a single path segment. Nested, trailing-slash, or empty shapes
are unserved (plain 404 before any authentication or store work — they
spend no bearer lookup or rate-limit budget).

## Wire shapes

All success bodies are JSON, with the catalogue wire's snake_case field
spellings throughout (`release_type`, `artist_mbid`, `art_url`, `sort_name`).

- `PUT /v1/me/library/albums/{mbid}` → `200 {"saved": true, "saved_at": "2026-11-08T00:00:00Z"}`.
- `GET /v1/me/library/albums/{mbid}` → `200 {"saved": <bool>, "saved_at": <ISO-8601 UTC string-or-null>}`.
- `DELETE …` → `200 {"saved": false, "saved_at": null}` (artists: `{"followed": false, "followed_at": null}`).
- `PUT /v1/me/library/artists/{mbid}` → `200 {"followed": true, "followed_at": <ISO>}`.
- `GET /v1/me/library/artists/{mbid}` → `200 {"followed": <bool>, "followed_at": <ISO-or-null>}`.
- `GET /v1/me/library/albums` → `200 [<entry>]` (bare array, catalogue convention):
  `{"mbid", "saved_at", "title", "release_type", "year", "artist_mbid", "artist", "art_url"}`.
  `art_url` is the own-origin `/v1/releases/{mbid}/art` path (same reserved
  path the catalogue emits; the art reader is not part of this slice).
  Mirror-absent enrichment degrades per field to `null` — the row is never
  silently dropped from the saved set.
- `GET /v1/me/library/artists` → `200 [<entry>]`:
  `{"mbid", "followed_at", "name", "sort_name", "type", "country"}` —
  same field spellings as the catalogue artist response.

`saved_at`/`followed_at` is the first successful save instant, ISO-8601 UTC
(`YYYY-MM-DDTHH:MM:SSZ`), rendered server-side with the same renderer as
`/v1/me/usage` (`crate::ledger::iso8601_utc`).

## Caps

- `{mbid}` must pass the catalogue MBID shape gate (`8-4-4-4-12` hex,
  case-insensitive). Anything else is a plain 404 before any store work —
  malformed and unknown targets are indistinguishable (no oracle).
- `GET …/albums` and `GET …/artists` return at most 50 entries (the same
  bound constant as the catalogue collections) as a bounded first page in
  `saved_at DESC, mbid ASC` order. Cursor paging is deliberately out of
  scope (the transport carries no query field today, same as the
  catalogue slice-1 lists); this contract will not silently add query
  parameters later.
- The total number of saved rows is not capped; the response is.

## Target validation (catalogue membership + mirror existence)

PUT and the state-GET resolve the target the same way before touching
saved state:

- **Album**: the release MBID must be a member of the caller's whole
  own-or-shared ingest ledger (the `library` table's `user_id = $1 OR
  user_id IS NULL` membership, same predicate the release detail uses —
  never limited to the first listing page) AND exist in the MusicBrainz
  mirror (`mb_release`). A well-formed MBID failing either check is a
  plain 404.
- **Artist**: the artist MBID must exist in the mirror (`mb_artist`) AND
  own at least one of the caller's member release MBIDs
  (`mb_release.artist_mbid`), i.e. the artist must be reachable through
  the caller's catalogue, not merely exist. Failure is a plain 404.

A target that is saved and later loses membership stays saved (rows are
never deleted by anything except the caller or account deletion): the
state-GET then answers 404 like any non-member target, DELETE stays
reachable, and the collection list keeps the row with degraded
enrichment. An absent/mirror-unknown source is never reported as saved.

## Authentication, scope, CSRF

- Every route requires an authenticated caller. Session cookie callers run
  the normal session check; a missing or invalid session answers the
  generic 401. Mutations (PUT/DELETE) on a cookie session additionally
  require the exact-`Origin` match (the existing mutation CSRF pin);
  a foreign or missing Origin answers 401 before any store work.
- Native-REST bearer credentials are scoped per ADR 0010's default-deny
  map: `GET` accepts `library:read`; `PUT` and `DELETE` accept the new
  `library:write` scope (this slice adds it to the scope vocabulary;
  existing credentials are unaffected and never carry it). A presented
  bearer lacking the scope answers the gate's denial like every other
  scoped route; a bearer-authorized caller skips the CSRF origin pin (a
  bearer-carrying request is never a cross-site form post).
- The caller's `user_id` always comes from the verified session/credential,
  never from the path or body — no cross-account state is expressible.
  Responses never carry the caller's id or any other account's state.

## Errors

Uniform, non-distinguishing failures:

- `401` generic auth body — unauthenticated; cookie-session mutation with a
  wrong/missing Origin.
- `404` — unknown path shape, malformed MBID, non-member or mirror-absent
  target on PUT/state-GET. `404` is also the plain answer for every
  unserved shape before authentication.
- `409 {"code":"account_deleting",…}` — a write (PUT/DELETE) while the
  caller's account deletion is scheduled; the in-statement writer fence
  refuses it. Reads are unaffected.
- `503` retryable `service_unavailable` — store outage on reads or the
  mirror probe; `500`/`503` opaque failure on writes. No response ever
  names the missing table, statement, or binding.
- Bearer scope denial follows the existing gate (`forbidden` envelope)
  only after authentication, like the other scoped routes.

## Storage (migration `0037_collection_saves`, app target)

`public.collection_saves (user_id, kind, mbid, saved_at)`, primary key
`(user_id, kind, mbid)`, `kind ∈ {'album','artist'}`, `mbid` TEXT (stored
verbatim after the Rust-side shape gate; no `::uuid` cast anywhere on the
app side), `saved_at` BIGINT unix seconds. `user_id` references
`public.users(id) ON DELETE CASCADE` (account deletion and the
unverified-user reap remove all preference rows — matching the
`listenbrainz_links`/`listen_events` preference precedent). Secondary
index `(user_id, kind, saved_at DESC, mbid)` backs the bounded list.

Every write statement carries the ADR 0002 rule-3 `writer_fence` on the
caller's user row (`SELECT id FROM users WHERE id = $n AND deleting_at IS
NULL FOR SHARE`): PUT's `INSERT … ON CONFLICT … DO UPDATE … RETURNING
saved_at` lands or replays in one statement (a replay keeps the first
`saved_at`), and DELETE removes `0..1` rows. All statements are
positional-parameter SQL over the app connection; the mirror probes for
target validation and list enrichment run over the mirror connection and
never receive an unvalidated MBID.

## Explicit non-goals

No track likes, no real-play history, no private taste export, no
playlists, no ListenBrainz/provider work, no ingest or playback side
effects, no UI, no cover-art caching, no supplementary (non-CC0)
MusicBrainz data — mirror reads stay on the CC0 `mb_*` columns the
catalogue already reads. Legacy `GET /api/library`, the ingest-play-meter
chain, and `velmora.media.v1` are byte-for-byte unchanged.
