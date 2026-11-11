# Subsonic client smoke procedure (DSub / Symfonium / Feishin)

Runs against staging once `workers/api` is deployed with the PlanetScale
`Library`. All three clients speak the same surface; the table below is the
acceptance checklist. Results are recorded here when the staging run lands
(slice 1: procedure only — no staging deployment exists yet).

## Setup (all clients)

- Server URL: staging `workers/api` origin + `/rest` path handling is
  server-side; enter the worker URL as the server address.
- Auth: per-client `apiKey` **without** `u`, `p`, `t` or `s`, or opt-in
  token auth with generated non-PII `u`, `t` and `s`. Never enter an account
  password or email. Plaintext `p=` and `p=enc:` must return error 42.
  Protocol version 1.16.1. Credential lifecycle/store wiring is still pending;
  this procedure does not claim it is deployed.
- Use a curated, library-member FLAC track and an entitled test account.
  Run only in an approved staging qualification window; the probes below are
  a procedure, not permission to contact a live service.

## Credential lifecycle (implementation; not staging evidence)

The native, session-only `/v1/subsonic-credentials` collection supports:

- `POST`: a fresh session (the native 600-second privilege window) and exact
  Origin are required. The JSON body contains a non-empty `label` (trimmed,
  at most 64 bytes, no control characters) and optional `auth_mode`:
  `apikey` (default), `token` or `both`. The generated username and applicable
  API key/token secret are shown only in the successful 201 response, after
  the database transaction commits. Responses are private/no-store.
- `GET`: an ordinary session lists at most 100 metadata rows, active first.
  No username, API key, HMAC, token secret or encrypted envelope is listed.
- `DELETE /v1/subsonic-credentials/{id}`: fresh session plus exact Origin;
  owner-only, idempotent revocation. Foreign and unknown IDs return the
  same 404. Reissue by creating a new credential then revoking the old one;
  lost secrets cannot be recovered.

The per-owner active limit is 32, serialized in the same transaction as the
insert. Issuance rechecks the selected live session and account under lock.
Session-compromise recovery also revokes compatibility credentials; its
production transaction revokes sessions and compatibility keys together,
so a session read before recovery cannot mint a key after that commit.

`SUBSONIC_INDEX_PEPPER` is read only as a Worker secret, never an ordinary
var or the native REST pepper. Its format is the bounded versioned keyring
in `workers/api/src/subsonic_config.rs`. Missing or invalid configuration
refuses issuance with 503 without exposing secret details or taking other
native routes down. Token modes reuse the exact-version vault KEK set and
the row/field-bound v2 envelope. No provisioning, credential generation or
migration application is authorized by this procedure.

The draft still mounts the empty compatibility store on `/rest/*`;
credentials created through these native endpoints do not yet enable
browsing or playback. The duplicate-rejecting query/form parser is written
but is not yet mounted. Its eventual transport must reject duplicate auth
parameters, including encoded names and copies split across query/form,
before constructing credentials.

Native `/v1/plays` now calls the shared, identity-based issuance core in
`workers/api/src/handlers.rs` (`issue_play_for_user`). It returns a typed
internal grant after the same subscription, hour-cap and one-user lease gates;
it does not authenticate compatibility credentials or enable a `/rest` stream.
A future compatibility adapter must check caller membership and curated-source
access first, then forward the grant privately to the native stream path. No
cookie-session fabrication, query grant or client scrobble can replace those
steps or delivered-byte metering. The shared ingest ledger's NULL owner is not
a caller's `library_tracks` membership; its visibility is not authorization
for compatibility catalogue, playlist entries or art.

## Checklist

| # | Client | Steps | Expect |
|---|--------|-------|--------|
| 1 | DSub (Android) | Add server → test connection; browse Artists → album → song; play | `ping` ok, indexes render, `stream` plays |
| 2 | DSub | Search "Test"; star a song; open Starred | `search3` match, `star`/`getStarred2` round-trip |
| 3 | DSub | Attempt Download; inspect `getUser`; disconnect | `download` refuses with error 0, `downloadRole=false`; no offered offline playback |
| 4 | Symfonium (Android) | Add Subsonic provider; sync library | `getArtists`/`getAlbumList2` paginate, art via `getCoverArt` |
| 5 | Symfonium | Create playlist, add song, reorder | `createPlaylist`/`updatePlaylist`/`getPlaylist` |
| 6 | Symfonium | Play queue persist across restart | `savePlayQueue`/`getPlayQueue` |
| 7 | Feishin (desktop) | Add server; full library scan | `getArtists`, `getAlbum`, `getSong` shapes accepted |
| 8 | Feishin | Scrobble a play; rate 5 stars | `scrobble`, `setRating` ok |
| 9 | All | Open an unsupported op path (`getVideos`) | Graceful "not supported" (error 0), no crash |
| 10 | All | Wrong API key | Clean auth failure (error 44), no retry storm |
| 11 | All | Send `p=` or `p=enc:` (GET and POST) | Error 42, even with another valid credential; no secret in any log sink |
| 12 | All | Play a non-member track or a track without a curated NNTP source | Error 70; no source contact or video fallback |
| 13 | All | Request `maxBitRate`, `format` or `timeOffset` on a playable member track | Launch FLAC bytes unchanged; private/no-store, no attachment disposition |
| 14 | All | Start playback on a second credential/device while the first is active | One concurrent stream; shared lease enforcement, not a `c=`-based bypass |
| 15 | All | Start playback without entitlement or after reaching the hour cap | Refused through the shared playback gate; no grant or source contact |
| 16 | All | Play an entitled member track, then inspect the shared play/hour ledger | Delivered audio is metered by the native path; a client scrobble is not metering evidence |

## Quick HTTP probes (pre-client sanity)

```bash
BASE=https://<staging-api>
# apiKey alone (v1 answers 43 if apiKey is mixed with u/p/t/s).
curl --connect-timeout 5 --max-time 15 "$BASE/rest/ping?apiKey=<key>&v=1.16.1&c=smoke"
curl --connect-timeout 5 --max-time 15 "$BASE/rest/getArtists?apiKey=<key>&v=1.16.1&c=smoke&f=json"
curl --connect-timeout 5 --max-time 60 -o /tmp/s.bin "$BASE/rest/stream?apiKey=<key>&v=1.16.1&c=smoke&id=<song-id>"
```

## Results

Pending staging deployment (PlanetScale `Library` follow-up).
