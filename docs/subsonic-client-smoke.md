# Subsonic client smoke procedure (DSub / Symfonium / Feishin)

Runs against staging once `workers/api` is deployed with the PlanetScale
`Library`. All three clients speak the same surface; the table below is the
acceptance checklist. Results are recorded here when the staging run lands
(slice 1: procedure only — no staging deployment exists yet).

## Setup (all clients)

- Server URL: staging `workers/api` origin + `/rest` path handling is
  server-side; enter the worker URL as the server address.
- Auth: username + per-client API key (hex). Protocol version 1.16.1.

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
