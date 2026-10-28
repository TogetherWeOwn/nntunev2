# Subsonic client smoke procedure (DSub / Symfonium / Feishin)

Run only after the credential release gates below and the PlanetScale
`Library` are proved on staging. The checklist is a target, not a claim that
these clients support the accepted API-key-only transport. Record the actual
client version and observed result; no authenticated staging or native-client
success is established by the native tests.

## Setup (all clients)

- Server URL: staging `workers/api` origin + `/rest` path handling is
  server-side; enter the worker URL as the server address.
- Auth: the approved, separately provisioned per-client OpenSubsonic `apiKey`,
  with **no `u` parameter, even empty**. Do not impose a new key alphabet or
  reinterpret the key as a username/password or native REST/session credential.
  Protocol version 1.16.1 is accepted and ignored by authentication.
- A client that sends the key as `p`, or insists on `u` alongside `apiKey`, is
  not compatible with this slice: record the refusal, not a successful smoke.
  Passwords (plain, encoded or malformed) are unsupported error 42; `apiKey`
  and `u` together answer 43. Token forms remain deny-only (missing salt 10,
  otherwise 40). No legacy fallback is enabled.

## Checklist

| # | Client | Steps | Expect |
|---|--------|-------|--------|
| 1 | DSub (Android) | Add server → test connection; browse Artists → album → song; play | `ping` ok, indexes render, `stream` plays |
| 2 | DSub | Search "Test"; star a song; open Starred | `search3` match, `star`/`getStarred2` round-trip |
| 3 | DSub | Download a song; play offline | `download` bytes, `audio/mpeg` |
| 4 | Symfonium (Android) | Add Subsonic provider; sync library | `getArtists`/`getAlbumList2` paginate, art via `getCoverArt` |
| 5 | Symfonium | Create playlist, add song, reorder | `createPlaylist`/`updatePlaylist`/`getPlaylist` |
| 6 | Symfonium | Play queue persist across restart | `savePlayQueue`/`getPlayQueue` |
| 7 | Feishin (desktop) | Add server; full library scan | `getArtists`, `getAlbum`, `getSong` shapes accepted |
| 8 | Feishin | Scrobble a play; rate 5 stars | `scrobble`, `setRating` ok |
| 9 | All | Open an unsupported op path (`getVideos`) | Graceful "not supported" (error 0), no crash |
| 10 | All | Wrong API key | Clean auth failure (error 44), no retry storm |

## Credential release boundary

The Worker now feeds the same native-tested request-scoped authenticator into
its dispatcher. Its storage adapter is wired to execute the native-visible
parameterized SQL plans through Hyperdrive only after release is allowed. The
new credential source is `subsonic_credentials`, never the inert legacy
`subsonic_clients` table. Joined owner state, API-key mode/scope, exact pepper
pairs, owner-fenced CAS and success-only last-used writes remain mandatory.
Native doubles, SQL-plan inspections and wasm compilation do **not** prove SQL
execution, concurrent locking or a deployed schema.

**Real credential success is still held.** The source-level release fence in
`workers/api/src/subsonic_runtime.rs` refuses runtime settings before reading
any pepper secret or limit configuration. No environment flag enables it. The
catalogue remains `MemStore::empty()` and does not implement PlanetScale
catalogue/Library or stream/download/cover-art delivery.

The limiter adapter claims the existing shared Postgres auth-rate-limit slots,
not an isolate-local map. `SUBSONIC_RATE_LIMITS` is an explicit JSON object with
required `general`, `auth_attempt_ip` and `credential` budgets, each containing
`max` and `window_secs`. There are no numerical defaults: missing, duplicate,
zero, malformed or out-of-store-bound settings deny. No deployed budgets or
configuration are established here. The pepper comes only from the
`SUBSONIC_INDEX_PEPPER` secret, with exact current/optional previous versioned
keys, never an ordinary-variable or scalar fallback. Typed keyring,
configuration, store and limiter faults use the existing error log emitter;
query strings, HMACs, keyring material and upstream diagnostics never enter it.
Emitted-event tests are not proof of live alert delivery.

Before a separately reviewed change can lift the release fence, prove the
forward schema and owner-disable state, supported authenticated scoped
issuance/revocation and secret placement, effective shared limits, independent
acceptance, exact-head security/CI/review and applicable staging gates. Also
prove zero unrevoked legacy credentials **and** that no legitimate legacy
writer/authenticator can introduce or use one between inventory and cutover.
A count alone is insufficient. No migration, credential insertion, secret
placement, rotation or retirement is authorized by this document. Source
schema work and deployed schema evidence must be reported separately.

## Quick HTTP probes (only after release prerequisites)

These are placeholders for a later approved staging run, not executable proof.
Never paste real keys into recorded commands, shell history, logs or evidence.

```bash
BASE=https://<staging-api>
# apiKey alone: no u; nonempty p/t/s mixed with a nonempty key also answer 43.
curl --connect-timeout 5 --max-time 15 "$BASE/rest/ping?apiKey=<key>&v=1.16.1&c=smoke"
curl --connect-timeout 5 --max-time 15 "$BASE/rest/getArtists?apiKey=<key>&v=1.16.1&c=smoke&f=json"
curl --connect-timeout 5 --max-time 60 -o /tmp/s.bin "$BASE/rest/stream?apiKey=<key>&v=1.16.1&c=smoke&id=<song-id>"
```

## Results

Pending credential-release prerequisites, authenticated DB-backed staging
proof, PlanetScale `Library`/byte integration and the recorded client checklist.
The 51 source-derived fixtures are not live seeded-v1 captures. Full catalogue,
per-user isolation, byte delivery, live captures and all three client runs remain
unverified; production Subsonic traffic stays unavailable.
