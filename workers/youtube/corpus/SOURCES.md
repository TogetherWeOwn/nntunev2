# Day-1 corpus sources

`day1.json` holds 100 videos: 50 `label_music` + 50 `audio_track`.

**Status: superseded pending re-cut (host research 2026-10-21).** This
file was built on VEVO-authorship + `- Topic` substring matching, which
the host's official-media selection research explicitly bans (NEVER
"Vevo"/"Topic" substring matching). Do not use it for gate numbers.
It stays in the tree as the harvested-ID pool only until the re-cut
lands; entries that pass the new verification may be kept, the rest go.

## Selection method (current)

Per `docs/youtube-backend-spike.md` (Official-media selection):

1. Key each recording by MBID + ISRC; resolve the ISRC first to the
   canonical artist, title and duration.
2. Search YouTube Music `songs` and `videos`; keep only
   `musicVideoType` OMV/ATV from
   `watchEndpointMusicConfig.musicVideoType`.
3. Gate by `channelId` allowlist + official-artist/verified badges.
4. Prefer the ATV's `/next` counterpart as the OMV (re-verified);
   else the top OMV passing duration (≈±5 s) and denylist checks.
5. MB video links / Wikidata P1651 only after liveness + OMV
   re-verification; IMVDb candidates always re-verified.
6. Audio from the exact-recording ATV at the best Opus (expect itag
   251), never from the OMV.

The re-cut runs from egress that is not bot-walled (Worker egress via
the runtime-entry slice, or the resi dev path) — dev-box egress sees
`LOGIN_REQUIRED` even for non-label videos (spec finding P1), so the
re-cut cannot run from here. `build.sh` fails closed until it is
rewritten to this method.

## Retired method (substring proxy, banned)

The previous `build.sh` harvested IDs from server-rendered search HTML
plus seeds, bootstrapped Topic tracks from related-video rails, and
classified through oEmbed (`VEVO` in author → `label_music`, author
ending ` - Topic` → `audio_track`). That proxy is retired; the script
body is kept for harvest-reference only and refuses to write a corpus.

## Stability

Video IDs are stable until an upload is deleted, privated or
region-restricted. The measurement run re-verifies membership (OMV/ATV
type + allowlist + badges + liveness) before using the corpus, and
records any drift in the spec's Day-1 readout. Corpus membership is
about *identity* (official OMV vs ATV); playability is what the spike
measures.
