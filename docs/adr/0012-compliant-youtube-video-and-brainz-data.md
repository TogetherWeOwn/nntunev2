# ADR 0012: Compliant YouTube video and Brainz data (PlayableSource, MBID-to-video join, licence corrections)

- **Status:** Proposed — two owner decisions are pending (spike
  retirement, §7; MetaBrainz supporter tier, §5). Both branches of each
  decision are drafted below. This record becomes Accepted only when
  those decisions close and the losing branches are struck.
- **Date:** 2026-11-03
- **Amends:** the licence terms in
  [0007-mb-mirror.md](0007-mb-mirror.md) (§5, §6 here; the companion
  edit to that record lands in the same change).

## Context

The product wants music videos and richer catalogue data. Three facts
constrain how:

1. **The existing `workers/youtube` spike cannot ship.** It extracts
   audio streams yt-dlp-style (Innertube client, stream-URL parsing,
   Range passthrough, planned residential proxy and signature
   solving). That breaks the YouTube API Services Developer Policies:
   III.I.7–8 (no separating audio/video), III.I.9 (no background
   player), III.I.14 (no access via other technology), III.E.1.a–b (no
   caching/offline copies), III.E.6 (no scraping), and the YouTube
   Terms of Service (no circumvention, no automated access). Policy
   texts read 2026-11-03; per the YouTube revision history the latest
   Developer Policies revisions are 2026-06-01 / 2026-05-04 and the
   Required Minimum Functionality (RMF) last changed 2025-07-07.
   Compliant use is the visible official IFrame player only.
2. **The mirror's licence story is wrong.** ADR 0007 assumes hourly
   MusicBrainz replication under "core data is CC0". In fact Live Data
   Feed replication packets are CC BY-NC-SA 3.0, and so are tags
   (including genre associations), ratings and annotations (the genre
   list itself is CC0 core; genre assignments to entities are not).
   Core data — artists, recordings, releases, release groups,
   relationships, URLs, ISRCs — is CC0, so YouTube URL relationships
   are free to use under either branch in §5. NNTune is a paid
   product, so non-commercial terms cannot be waved through.
3. **There is no curated MBID→YouTube dataset to import.**
   ListenBrainz keeps no MBID→YouTube mapping (verified in its code:
   YouTube IDs there come from individual listens or from play-time
   YouTube search). MusicBrainz relationships are the primary source,
   and our measurement (§8) shows they cover only ~1.6% of sampled
   album recordings — the join is necessary but nowhere near
   sufficient, so curation is a first-class part of this design.

The model we follow is ListenBrainz's BrainzPlayer *interface shape*
(`DataSourceType`: `play`, `pause`, `seek`, fallback on failure). Its
code is GPL-licensed and this repo is MIT: model the shape, copy no
code. Two of its behaviours are explicitly not adopted: play-time
`search.list` (§2), and any player mode that keeps YouTube playing
while not visible (§1).

## Decision

### 1. `PlayableSource` abstraction

A track is keyed by recording MBID (ISRC secondary). Playback goes
through one interface: `kind`, `canPlay(track)`, `play`, `pause`,
`seek`, `onEnd`, `onError` (advances to the next playable source),
`recordsListens`. Two implementations:

- **`NntpSource`** — audio from the user's own provider. The default.
  Background play, gapless, offline and Media Session are allowed:
  none of YouTube's rules apply to it.
- **`YouTubeEmbedSource`** — video only, through the official IFrame
  Player API, always visible. Hard rules, each testable:
  - Player viewport at least 200×200, target 480×270; no overlays,
    frames or elements in front of any part of it; YouTube controls,
    ads and branding untouched; `origin` set.
  - At most one autoplaying player per screen, autoplay only when
    more than half of it is in view (RMF).
  - On `visibilitychange` to hidden, the source **pauses** (or the
    queue skips to an NNTP item). No hidden, minimised-to-audio-bar,
    background or lock-screen playback, and no Media Session
    registration for YouTube items (III.I.9).
  - Never muted-behind-our-audio: syncing a muted YouTube video to
    NNTP audio is an alternate audio track and is banned (III.I.7).
    There is therefore no "switch to video mid-track" continuity;
    watching the video is a separate playback of that video.
  - Not behind a paywall: YouTube playback must work on the free
    plan (III.F.3.a); no gating step beyond the play click except
    login/age checks (III.F.3.b); no rewards for watching.
  - Our terms bind users to the YouTube Terms of Service, and the
    privacy policy references the Google Privacy Policy and
    discloses API data use (III.A), before the feature is used.
- Mixed-source queues carry per-item source attribution; non-YouTube
  items are never presented as YouTube results and YouTube items are
  never presented as ours (III.C.5, III.F.2.a–b). When the next queue
  item is YouTube, the video surface is shown. No clause grants
  explicit permission for mixed queues, so this attribution and
  visibility reading is the conservative one and is listed for
  re-check at every policy revision.
- Present YouTube as a *video* feature, never as an audio source.
  III.I.1 (substitute for a YouTube application) and III.E.1
  (facilitating infringement) are residual audit risks for a
  Usenet-based paid app; a visible, clearly-attributed video surface
  is the mitigation available to us. This stays a launch-gate legal
  item, not a settled permission.

### 2. Recording MBID → YouTube video join and curation

Join chain (all CC0 core data, served from the mirror):

```
audio recording ──(recording–recording "music video",
                   ce3de655-7451-44d1-9224-87eb948c205d)──▶ video recording
video recording ──(recording–URL "free streaming" + video attribute,
                   7e41ef12-a124-4324-afdb-fdbae687a89c)──▶ YouTube URL → video ID
```

A direct `free streaming` YouTube URL relationship on the audio
recording itself is also accepted (it accounted for a comparable
share of the measured hits, §8). Fallbacks and curation, in priority
order:

1. MusicBrainz URL/recording relationships (above).
2. Wikidata P1651 (YouTube video ID, CC0), via the recording/work's
   Wikidata link where one exists (0 of 500 sampled recordings had a
   recording-level Wikidata URL, §8 — treat as opportunistic).
3. YouTube IDs observed in ListenBrainz listens/playlists
   (`additional_info.youtube_id` / `origin_url`).
4. Moderated user submissions: a review queue, provenance recorded,
   and the submitter is nudged to add the link to MusicBrainz so the
   commons gains it.
5. Optional future source (e.g. IMVDb) only after its licence/terms
   are checked; not in scope here.

Validation and storage:

- Every candidate ID is batch-validated with
  `videos.list?part=status,contentDetails` (≤50 IDs per call, 1
  quota unit; 10,000 units/day project quota) checking
  `status.embeddable`, privacy status and region restrictions, in a
  background curation job — never on the play path.
- **`search.list` is never called at play time** (its own bucket is
  100 calls/day). If used at all, only inside an admin curation
  tool.
- Storage distinguishes provenance. IDs obtained from non-API
  sources (MusicBrainz, Wikidata, ListenBrainz, moderated
  submissions) are not YouTube API Data and may be stored with their
  source and verification timestamp. Anything returned by the
  YouTube API (status, titles, and any IDs found via search) **is**
  API Data: III.E.4.c–d require it be refreshed or deleted within
  30 days. A TTL job enforces this; the stored row is ID +
  provenance + `verified_at` + the check outcome only, and the
  check outcome expires at 30 days and re-validates on next use.
- Live MusicBrainz API use (outside the mirror) stays ≤1 req/s with
  a `nntune/<ver> ( contact )` User-Agent; reads serve from the
  mirror.

The mirror schema this needs (release groups, recording-URL and
artist-URL relationships, recording–recording `music video` links,
ISRCs) is a separate build change on top of
[0007-mb-mirror.md](0007-mb-mirror.md); this ADR is the contract it
implements. Artist-level surfaces (a Videos shelf from the artist's
video recordings; `youtube` / `youtube music` channel and "Listen
on…" links from artist URL relationships) are part of the same
contract and, per §8, are the primary surface — per-track video is
opportunistic until coverage grows.

### 3. Web security surface (CSP and embed)

Today's CSP (`apps/web/vite.config.ts`) is nonce-mode with
`script-src 'self'` and no `frame-src`. The compliant embed needs,
and the security review must approve before any build lands:

- `frame-src https://www.youtube-nocookie.com` (privacy-enhanced
  embed host).
- `script-src` additionally `https://www.youtube.com` for the IFrame
  Player API (`/iframe_api` and its widget script; plus
  `https://s.ytimg.com` only if the loader is verified to require it
  at build time), because §1's
  pause-on-hidden and end/error events require that API. No
  `'unsafe-inline'` or `'unsafe-eval'` is added. This is a real
  weakening of a currently strict directive: it is scoped to that
  one origin, and the implementation must evaluate route-scoping the
  relaxation to video surfaces; if the framework CSP cannot be
  route-scoped, that fact and the residual risk are recorded in the
  security review, not silently accepted here.
- The embed breaks without a Referer (RMF client identification;
  error 153), while the app-wide `Referrer-Policy: no-referrer` is
  load-bearing for magic-link tokens. Fix per-iframe only:
  `referrerpolicy="strict-origin-when-cross-origin"` on the embed
  element (sends origin, never path or token), and **never render an
  embed on the magic-link landing route**. The app-wide policy is
  unchanged.
- **No YouTube bytes through our origins.** Thumbnails are YouTube
  content (III.E.1.a, III.I.4, III.F.2.a): never cached, proxied or
  stored in R2. v1 shows no YouTube thumbnails outside the embed
  itself, so `img-src` is unchanged; adding `*.ytimg.com` later
  needs its own review.
- Workers never fetch YouTube media. The only server-side YouTube
  calls are `videos.list` validation from the curation job.
- The YouTube Data API key is a new scoped credential (free quota),
  minted and stored through the existing scoped-credentials path,
  referenced only by the curation job. It is never shipped to the
  browser.

### 4. ListenBrainz uses

Adopted, all client-visible reads proxied/cached through our API
with `X-RateLimit-*` headers respected:

- LB Radio (`/1/explore/lb-radio`, needs the user's token),
  created-for-you playlists (`/1/user/<u>/playlists/createdfor`),
  fresh releases, popularity (`/1/popularity/...`), Labs similar
  artists/recordings, and scrobbling for linked accounts.
- ListenBrainz dumps are CC0; commercial use is allowed with
  financial support strongly urged (a request, not a licence term).
  MLHD+ is non-commercial only and is never used.
- A user's ListenBrainz token is a credential: stored via the
  existing vault/credentials mechanism
  ([0010-native-rest-credentials.md](0010-native-rest-credentials.md)),
  never logged, sent only to ListenBrainz. Account linking asks for
  username + token explicitly; every ListenBrainz feature degrades
  to a signed-out empty state without one.
- ListenBrainz is not a source of curated video links (§2 order 3 is
  listen-observed IDs only). Anyone proposing "import the
  ListenBrainz MBID→YouTube map" is proposing something that does
  not exist.

### 5. MetaBrainz licence correction — owner decision, both branches

Fact (both branches): hourly replication packets and
tags/genre-associations/ratings/annotations are CC BY-NC-SA 3.0;
core data including URL relationships is CC0. ADR 0007's terms are
corrected in the companion edit regardless of branch.

- **Branch A — supporter tier approved.** Stealth Start-Up ($0)
  while the product is not public; Bronze ($100/month and up) from
  public launch, which licenses the commercial use. Hourly
  replication (ADR 0007 point 3, lag SLO <2 h) and tags/genres may
  be used under that agreement. The tier is an operating cost of
  the mirror, reviewed at launch and on tier changes.
- **Branch B — no tier (CC0-only mirror).** The mirror switches to
  the twice-weekly CC0 core dumps (plus the CC0 canonical dumps on
  the 1st/15th if useful); hourly replication stops, and with it
  ADR 0007's <2 h lag SLO is replaced by dump freshness (≤4 days).
  NC data is excluded at import: no tags/genre associations,
  ratings, annotations, or derived statistics built from them.
  Browse-by-genre then needs another source (Wikidata P136, CC0,
  coverage unmeasured) or is deferred. Everything §2 needs is CC0
  core, so video is unaffected by this branch.

Either branch: CritiqueBrainz reviews are filtered per-review to
CC BY-SA only (pre-2022 imports may be NC); AcousticBrainz (frozen
2022) may supply static features but is never a dependency;
Wikipedia summaries are CC BY-SA 4.0 and carry attribution; synced
lyrics stay feature-flagged **off** (no stated licence at the free
source; publisher rights are the blocker) pending a separate
licensing decision.

### 6. Cover-art rights flag

ADR 0007 point 6 claimed CAA images "follow upstream licence terms
per image". No such per-image licences exist: the Cover Art Archive
states images are copyrighted by their respective owners and grants
no licence. Corrected position, recorded here and in ADR 0007:

- Caching CAA art in R2 and serving it inside a **paid** product is
  common industry practice but is **not licensed**. This is an open
  legal risk, explicitly on the pre-launch legal-review list; it is
  not cleared by this ADR.
- Until that review: art is cached unchanged (no relicensing claims,
  no use in marketing), the mirror stores CAA URLs/hashes, and a
  takedown path (drop the cached object and the URL) must exist
  before public launch.
- This flag is about CAA only. YouTube thumbnails are dealt with in
  §3 (never stored at all).

### 7. Retiring the `workers/youtube` extraction spike — owner decision, both branches

The spike was approved 2026-10-21. It is non-compliant (Context 1).
Neither branch of this ADR authorizes extraction as a
`PlayableSource`, a deployed service, or a fallback: the only
question the owner decision settles is disposal.

- **Branch A — retirement approved.** Retire plan, one change set:
  1. Remove `workers/youtube` (code, `wrangler.toml.example`
     contract, corpus) and `docs/youtube-backend-spike.md`; remove
     any deployed spike worker, its KV namespaces (`YT_CONFIG`,
     `YT_PLAYER_CACHE`) and Durable Object, and cancel the
     residential-proxy arrangement (~$4/GB).
  2. Preserve, rewritten fresh if retained, only the pure curation
     logic in `select.rs` that §2 still uses (duration match,
     denylist, official-video preference). Drop anything derived
     from Innertube (e.g. `musicVideoType`).
  3. The CI grep gate (no `googlevideo`, `/youtubei/`, `yt-dlp`,
     `youtubei` references) must pass on the removal and stay green.
  4. Git history keeps the spike; that is a record, not a live path.
     No history rewrite.
- **Branch B — retirement declined (for now).** The spike is frozen
  in place and quarantined: no deploys, no wiring into
  `PlayableSource` or any user-facing route, no new proxy spend,
  and its README/spec carry a "non-compliant, do not ship" banner
  pointing here. The compliant embed design (§1–§4) proceeds
  unchanged. Revisit at public launch at the latest; a public
  launch with a live extraction path is out of bounds under this
  ADR under either branch.

## Measurement: MusicBrainz video-link coverage (2026-11-03)

Question: for recordings a user might play, how often does the §2
MusicBrainz join reach a YouTube ID?

Method (read-only, live MusicBrainz API, ≤1 req/s, identifying
User-Agent, 160 calls): 26 letter shards of official album releases
(`release:<letter>* AND status:Official AND primarytype:Album`, two
relevance windows per shard, offset 0 and 50, 2 releases each = 104
releases); browse each release's recordings with
`inc=url-rels+recording-rels+isrcs` (pool: 1,332 unique non-video
recordings); deterministic sample of 500 ordered by
`sha256(recording MBID)`; then one lookup per `music video` target
(4) for its URL relationships.

| Measure | Count | Share |
|---|---:|---:|
| Sample (unique non-video recordings) | 500 | — |
| Has a `music video` recording relationship | 4 | 0.8% |
| …whose video recording links to YouTube (`free streaming`) | 4 | 0.8% |
| Direct `free streaming` YouTube URL on the audio recording | 5 | 1.0% |
| **Usable YouTube ID via MusicBrainz (either path)** | **8** | **1.6%** (Wilson 95% CI 0.8–3.1%) |
| Any `free streaming` URL (any service) | 49 | 9.8% |
| Has ISRC | 246 | 49.2% |
| Recording-level Wikidata URL (P1651 potential) | 0 | 0% |

All 8 usable hits were among the 246 ISRC-bearing recordings (3.3%
of that subset). Limits: the shard windows bias toward
search-visible albums, album tracks are not singles (singles
plausibly carry more video links), and coverage was not listener-
weighted — popular tracks are likely better covered than the raw
catalogue. Read 1.6% as "low single digits for arbitrary album
tracks", not as a popularity-weighted product forecast.

Consequences baked into the decision above: the Music Video surface
is empty-state-first; the artist Videos shelf (§2) is the primary
surface; the curation pipeline (fallbacks 3–4) is not optional
polish; and no launch or marketing claim may promise video for an
arbitrary track.

## Rejected alternatives

1. **Ship the extraction spike as a source.** Non-compliant
   (Context 1) regardless of its earlier approval; an ADR cannot
   launder a terms breach.
2. **Play-time YouTube search** (the BrainzPlayer fallback). Dies on
   the 100-calls/day `search.list` bucket and makes playback depend
   on quota.
3. **Reuse BrainzPlayer code.** GPL in an MIT repo; and its
   search-at-play behaviour is rejected above. Interface shape only.
4. **Proxy/cache YouTube thumbnails or media via R2/Workers.**
   Banned (III.E.1.a, III.I.4); §3 instead.
5. **Keep claiming the hourly feed is CC0.** Factually wrong (§5);
   the choice is to license it (A) or stop using it (B).
6. **Hide-the-player audio mode / muted synced video.** III.I.9 /
   III.I.7. Not a design option at any price.

## Consequences

- The player gains a source abstraction whose compliant branch is
  visible-video-only; audio features (background, gapless, offline)
  stay NNTP-only, permanently. UX must not blur that line.
- Video ships as an opportunistic, attribution-labelled surface
  (free plan included), gated on: the §3 CSP security review, the
  two owner decisions, terms/privacy updates (III.A), and the
  30-day TTL job. The CI guardrails (grep gate; no `search.list` on
  the play path) are acceptance criteria for the build changes,
  which also re-check the YouTube policy revision history — these
  policies change, and this ADR cites the revisions read above.
- Follow-on build work, each its own change: mirror schema
  extension (§2); `PlayableSource` + `YouTubeEmbedSource` + video
  surfaces (§1); curation job + moderated submissions (§2);
  ListenBrainz account link (§4); spike disposal (§7 branch A) or
  quarantine banner (§7 branch B).
- Open risks carried, not closed: III.I.1/III.E.1 audit exposure
  (§1), unlicensed CAA art in a paid product (§6), Wikidata P136
  genre coverage (§5 branch B). All three are pre-launch items.
