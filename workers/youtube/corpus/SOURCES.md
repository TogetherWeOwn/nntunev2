# Day-1 corpus sources

`day1.json` holds 100 videos: 50 `label_music` + 50 `audio_track`.

Scope note (owner 2026-10-21): the spike covers official music videos
only (externally curated/verified), preferring the official track at
the best available quality for audio. VEVO authorship + `- Topic`
auto-tracks are the standing proxy for "official" until the host posts
the official-media selection research; re-run `build.sh` with new
selection criteria if that research requires it.

## Method (`build.sh`)

1. Phase 1: harvest candidate video IDs from server-rendered YouTube
   search HTML (several music queries per class) plus a small seed
   list of known-good label videos.
2. Phase 2 (only if `audio_track` is short): bootstrap from the
   related-video rails of already-found Topic watch pages — search
   relevance buries Topic uploads, while related rails run ~45%
   Topic. Capped at 15 watch-page fetches.
3. Verify + classify each candidate through oEmbed (no key, cacheable):
   - author contains `VEVO` → `label_music` (VEVO = label-owned upload)
   - author ends with ` - Topic` → `audio_track` (auto-generated track)
   - anything else, or an oEmbed miss → skipped (counted, not stored)
4. Keep the first 50 per class, sorted by category + video ID.

Every entry carries `title`, `author`, `verified_at` (UTC date) and
`source`. Re-run `build.sh` to refresh; it exits non-zero unless both
classes reach 50.

## Stability

Video IDs are stable until an upload is deleted, privated or
region-restricted. The measurement run re-verifies membership (oEmbed
200 + same class) before using the corpus, and records any drift in the
spec's Day-1 readout. Corpus membership is about *identity* (label vs
Topic); playability is what the spike measures.
