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
