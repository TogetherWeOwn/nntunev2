# ADR 0012: Compliant YouTube video and Brainz data (PlayableSource, MBID-to-video join, licence corrections)

- **Status:** Accepted — both owner decisions closed 2026-11-03:
  the spike is kept as a follow-on track with embeds first (§7), and
  MetaBrainz data is free/CC0-only with no supporter tier (§5).
- **Date:** 2026-11-03
- **Amends:** the licence terms in
  [0007-mb-mirror.md](0007-mb-mirror.md) (§5, §6 here; the companion
  edit to that record lands in the same change).

## Context

The product wants music videos and richer catalogue data. Three facts
constrain how:

1. **The existing `workers/youtube` spike is not a compliant
   shipping path.** It extracts audio streams yt-dlp-style (Innertube
   client, stream-URL parsing, Range passthrough, planned residential
   proxy and signature solving). That breaks the YouTube API Services
   Developer Policies: III.I.7–8 (no separating audio/video),
   III.I.9 (no background player), III.I.14 (no access via other
   technology), III.E.1.a–b (no caching/offline copies), III.E.6 (no
   scraping), and the YouTube Terms of Service (no circumvention, no
   automated access). Policy texts read 2026-11-03; per the YouTube
   revision history the latest Developer Policies revisions are
   2026-06-01 / 2026-05-04 and the Required Minimum Functionality
   (RMF) last changed 2025-07-07. The compliant path — and the only
   YouTube playback path this record authorizes for users — is the
   visible official IFrame player. The owner has nevertheless kept
   the spike as a later track (§7): that decision carries the
   policy/ToS risk above as an accepted owner risk behind the §7
   gates; it does not make extraction compliant.
2. **The mirror's licence story is wrong.** ADR 0007 assumes hourly
   MusicBrainz replication under "core data is CC0". In fact Live Data
   Feed replication packets are CC BY-NC-SA 3.0, and so are tags
   (including genre associations), ratings and annotations (the genre
   list itself is CC0 core; genre assignments to entities are not).
   Core data — artists, recordings, releases, release groups,
   relationships, URLs, ISRCs — is CC0, so YouTube URL relationships
   are free to use. The owner (2026-11-03) framed NNTune as a
   private, invite-only, non-commercial hobby service and chose the
   free CC0-only path (§5): no supporter tier, no account or token,
   and no reliance on the service's non-commercial framing to use
   NC-licensed data.
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

**REA angle:** study ListenBrainz's BrainzPlayer to learn how its
YouTube source behaves in a mixed queue (visibility, end/error
advance, fallback) — open source first, REA on the shipped web
bundle only for gaps. That study is done (2026-11-03) and its
findings are folded into §1 (adopted / not-adopted list) and §3
(server-side key); it was input to the §1 build, not a blocker for
this record. The same study-first rule applies to the §7 proxy
question before any proxy workaround is bought.

## Decision

### 1. `PlayableSource` abstraction

A track is keyed by recording MBID (ISRC secondary). Playback goes
through one interface: `kind`, `canPlay(track)`, `play`, `pause`,
`seek`, `onEnd`, `onError` (advances to the next playable source),
`recordsListens`. Two implementations ship under this record.
(`YouTubeEmbedSource` is the first YouTube path built and the
default whenever YouTube plays; the kept extraction spike may only
ever become a third implementation under §7's gates.)

**Resolver order (owner decision, 2026-11-03).** For a track the
resolver tries, in order:

1. **Curated FLAC over NNTP** (`NntpSource`) — the NNTP step itself
   resolves the R2 popularity cache first, then the backend
   provider pool (ADR 0013/0014, landing separately).
2. **YouTube embed** (`YouTubeEmbedSource`), when a validated video
   ID exists (§2).
3. **Unavailable** — hidden by default. An admin setting may show
   unavailable tracks greyed, with a future "request this song"
   action instead of Play; neither changes the resolver.

Users never see NNTP or providers: surfaces say only whether a
track plays. Implementing this resolver is gated on ADR 0013/0014
landing and reconciling the BYOP-era contracts they supersede
(the README's bring-your-own-provider vision and ADR 0003's
per-user serve grants, both still in the tree); until then this
section describes the target model, not today's build.
Separately from the audio resolver, **YouTube is the
music-video source for all tracks**, NNTP-streamed ones included:
watching the video is its own visible playback of that video (no
mid-track switch — see the muted-sync ban below), not a mode of
the audio player.

- **`NntpSource`** — audio from the curated collection over NNTP,
  served through backend provider accounts. The default. There is
  no user provider anymore. Background play, gapless and Media
  Session are allowed: none of YouTube's rules apply to it.
  Offline download is not offered at launch on any source.
- **`YouTubeEmbedSource`** — official videos through the official
  IFrame Player API, always visible. It serves both resolver step
  2 and the all-tracks music-video rule above, and in both roles it
  is presented as watching that video on a visible video surface —
  as the audio fallback it is still that video playing visibly,
  never a hidden or audio-only presentation. Hard rules, each
  testable:
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
  - No separate charge or gate for YouTube playback: there is no
    free plan at launch — the whole service is invite-only and
    subscription-gated — so III.F.3.a is restated for that shape:
    YouTube playback is not separately charged for, upsold or
    gated beyond access to the service itself; no gating step
    beyond the play click except login/age checks (III.F.3.b);
    no rewards for watching. Whether that reading satisfies
    III.F.3.a stays a launch-gate legal item; this record does not
    assert a free-plan path.
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
  Resolver step 2 does not change that presentation: the fallback
  is the visible video playing on the video surface, labelled as
  YouTube, with no hidden, minimised or audio-only mode. III.I.1
  (substitute for a YouTube application) and III.E.1 (facilitating
  infringement) are residual audit risks for a Usenet-based app;
  a visible, clearly-attributed video surface is the mitigation
  available to us, and the III.I.1 reading of the fallback role is
  kept on the launch-gate legal list. This stays a launch-gate
  legal item, not a settled permission.
- BrainzPlayer study (done 2026-11-03), adopted as build input:
  per-track source fall-through in resolver order with a 3 s
  ready time-out; one reused player instance; embed errors
  2/5/100/101/150 → toast and skip; the played watch URL written
  back onto the recorded listen; initial window sizing (floating,
  bottom-right, 10 px above the 60 px bar; default and minimum
  350×200; expanded min(1280, available) wide at 16:9 plus a 30 px
  button row, capped at viewport − 120 px tall). Not adopted:
  play-time `search.list` (§2); close-as-hide-and-pause (close
  stops the playback); keeping the player hidden while YouTube is
  not the current source; cover art over the video on mobile
  full-screen (the no-overlay rule above); no pause on a hidden
  tab (we pause, above); the standard embed host (§3 uses
  no-cookie); thumbnails over http (v1 shows none, §3);
  fall-through that skips sources before the starting one (ours
  always starts at resolver step 1); and native-controls-off
  player chrome — §1's controls/branding-untouched rule stands,
  and any `controls` change needs its own RMF check at build time.

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

- MusicBrainz, Wikidata, ListenBrainz and user-submitted URLs are
  crowd-sourced input: the embed URL is constructed only from a
  validated video ID (strict 11-char `[A-Za-z0-9_-]` form), and a
  source URL is never passed through into the iframe `src`.
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
- The YouTube Data API key (free quota) is a retrievable outbound
  credential, so it is **not** an
  [0010-native-rest-credentials.md](0010-native-rest-credentials.md)
  scoped credential — those are inbound bearer keys stored only as
  one-way HMACs and cannot hold a key we must present to Google.
  The key lives as a Cloudflare Worker secret (the pattern
  [0008-auth-vault-sessions.md](0008-auth-vault-sessions.md)
  already uses for the vault KEK), is restricted in Google Cloud
  to the YouTube Data API v3 only, and is referenced only by the
  curation job. It is never shipped to the browser: the embed
  itself needs no key, and ListenBrainz's approach of putting one
  project key into every page — logged-out visitors included, all
  sharing one quota — is explicitly not copied. Creating the
  Google Cloud project and key is a credential-creation step that
  needs its own owner/operator route at build time; app code never
  mints it, and this record authorizes neither the account nor
  the key.

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
- A user's ListenBrainz token is a retrievable upstream credential,
  so it is stored in the BYOP vault
  ([0008-auth-vault-sessions.md](0008-auth-vault-sessions.md):
  AES-256-GCM envelope, per-user DEK, versioned KEK) — the same
  treatment as the user's Usenet provider password. It is
  explicitly **not** an
  [0010-native-rest-credentials.md](0010-native-rest-credentials.md)
  credential: those are server-issued keys kept only as one-way
  HMACs, which cannot be presented upstream. Contract for the
  account-link build: the token is decrypted only server-side to
  call ListenBrainz, is never returned to the client after saving
  and never logged; unlinking the account deletes the vault entry
  in the same operation, and re-linking replaces it (rotation is
  delete-then-store, never an in-place edit of a second copy).
  Account linking asks for username + token explicitly; every
  ListenBrainz feature degrades to a signed-out empty state
  without one.
- ListenBrainz is not a source of curated video links (§2 order 3 is
  listen-observed IDs only). Anyone proposing "import the
  ListenBrainz MBID→YouTube map" is proposing something that does
  not exist.

### 5. MetaBrainz licence correction — free, CC0-only (owner decision, 2026-11-03)

Fact: hourly replication packets and
tags/genre-associations/ratings/annotations are CC BY-NC-SA 3.0;
core data including URL relationships is CC0. ADR 0007's terms are
corrected in the companion edit.

**Decision (owner, 2026-11-03): free only — no MetaBrainz supporter
tier, no MetaBrainz account, and no token.** NNTune is a private,
invite-only, non-commercial hobby service; that framing is the
reason for the choice, and it is recorded here so the choice is not
quietly relitigated. The mirror therefore runs CC0-only:

- The mirror uses the twice-weekly CC0 core dumps (plus the CC0
  canonical dumps on the 1st/15th if useful); the hourly Live Data
  Feed is dropped from ADR 0007's mirror design, and ADR 0007's
  <2 h lag SLO is replaced by a dump-freshness target (≤4 days;
  no dump-age monitor or pager exists yet, so nothing may claim it
  is a monitored SLO until they are built).
- NC data is excluded at import: no tags/genre associations,
  ratings, annotations, or derived statistics built from them.
- Browse-by-genre needs a CC0 source. Wikidata (P136) is the
  candidate but its coverage is unmeasured — an **open question**,
  not a commitment; genre browsing is deferred until it is answered.
- Everything §2 needs is CC0 core, so video is unaffected.
- **Re-open trigger:** the service starts earning real revenue and
  the benefits justify paying. That is a new owner `Money:`
  decision at that time; nothing here plans for it.
- **Open question (not decided, kept out of this decision):**
  whether a non-commercial service may use NC-licensed data — and
  the free non-commercial feed token — under CC BY-NC-SA. That is a
  legal question and the token would be a credential; this record
  takes no position and authorizes neither.

Also: CritiqueBrainz reviews are filtered per-review to CC BY-SA
only (pre-2022 imports may be NC); AcousticBrainz (frozen 2022)
may supply static features but is never a dependency; Wikipedia
summaries are CC BY-SA 4.0 and carry attribution; synced lyrics
stay feature-flagged **off** (no stated licence at the free source;
publisher rights are the blocker) pending a separate licensing
decision.

### 6. Cover-art rights flag

ADR 0007 point 6 claimed CAA images "follow upstream licence terms
per image". No such per-image licences exist: the Cover Art Archive
states images are copyrighted by their respective owners and grants
no licence. Corrected position, recorded here and in ADR 0007:

- Caching CAA art in R2 and serving it in the product is common
  industry practice but is **not licensed** — the service's private,
  non-commercial framing (§5) does not create a licence either.
  This is an open legal risk, explicitly on the pre-launch
  legal-review list; it is not cleared by this ADR.
- Until that review: art is cached unchanged (no relicensing claims,
  no use in marketing), the mirror stores CAA URLs/hashes, and a
  takedown path (drop the cached object and the URL) must exist
  before public launch.
- This flag is about CAA only. YouTube thumbnails are dealt with in
  §3 (never stored at all).

### 7. The `workers/youtube` extraction spike is kept — embeds first (owner decision, 2026-11-03)

The spike was approved 2026-10-21. It is non-compliant (Context 1).
The owner (2026-11-03) declined to retire it: embeds are built and
shipped first, and the spike stays as the follow-on track toward
playback that does not depend on embeds forever. This record is
precise about what that does and does not authorize:

- **Embed first and default.** `YouTubeEmbedSource` (§1) is the
  first YouTube path built and remains the default. The spike does
  not gate, delay, or shape the embed work.
- **Kept, behind a flag, off in production.** If the spike is ever
  wired in, it is as a later third `PlayableSource` behind a
  feature flag that is off in production. Until the gates below
  close it is not deployed as a user-facing path, not a fallback
  when the embed fails, and not reachable from any user route.
- **The policy/ToS risk is an accepted owner risk, recorded, not
  retired.** Context 1 stands unchanged: extraction breaks the
  YouTube API Services Developer Policies and the YouTube Terms of
  Service. Keeping the spike accepts that exposure at owner level;
  it does not make the technique compliant, and no build change may
  cite this section as compliance cover. Turning the flag on for
  users needs a fresh owner decision made against the policy texts
  current at that time.
- **No residential proxy is approved.** The owner kept the spike,
  not the spend: the ~$4/GB residential arrangement is not
  authorized by this record or by the keep decision. Any proxy use
  needs its own owner `Money:` card with a monthly cap, and the
  owner's proxy order of preference (2026-11-03) applies when that
  card is written:
  - **A (preferred):** no residential proxy at all;
  - **B:** proxy only for auth/extraction (`/player`), never for
    media bytes — already the spike's design (`workers/youtube`
    README: proxy only for `/player` when the extraction IP is
    bot-walled). Its GO/NO-GO gate is the measure: if more than 5%
    of byte fetches from Worker egress fail, bytes would need the
    proxy too, which is uneconomic — a NO-GO for this option;
  - **C:** a much cheaper proxy, only if bytes demonstrably need
    one.
  Measuring whether A or B holds is follow-on research that starts
  only after embeds work; per the REA angle above, the study comes
  before any spend.
- **Hygiene while kept:** the pure curation logic §2 still uses
  (duration match, denylist, official-video preference) may be
  reused, rewritten fresh where it is entangled with Innertube
  (e.g. `musicVideoType` is dropped, not carried over). Nothing in
  the kept spike weakens §3: the curation job's `videos.list` calls
  remain the only server-side YouTube calls this record authorizes
  while the flag is off.

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

1. **Ship the extraction spike now, as the YouTube path.**
   Non-compliant (Context 1) regardless of its earlier approval; an
   ADR cannot launder a terms breach. The owner kept the spike as a
   later, flag-off track (§7) — shipping it as the path now was not
   chosen, and retiring it outright was also declined.
2. **Play-time YouTube search** (the BrainzPlayer fallback). Dies on
   the 100-calls/day `search.list` bucket and makes playback depend
   on quota.
3. **Reuse BrainzPlayer code.** GPL in an MIT repo; and its
   search-at-play behaviour is rejected above. Interface shape only.
4. **Proxy/cache YouTube thumbnails or media via R2/Workers.**
   Banned (III.E.1.a, III.I.4); §3 instead.
5. **Keep claiming the hourly feed is CC0**, and **license it via a
   supporter tier** (Stealth now, Bronze at launch). The first is
   factually wrong (§5); the second was put to the owner and
   rejected — free/CC0-only is the decision, re-openable only on
   real revenue (§5).
6. **Hide-the-player audio mode / muted synced video.** III.I.9 /
   III.I.7. Not a design option at any price.

## Consequences

- The player gains a source abstraction whose compliant branch is
  visible-video-only; audio features (background, gapless, Media
  Session) stay NNTP-only, permanently, and offline download is
  not offered at launch on any source. UX must not blur that line.
- Video ships as an opportunistic, attribution-labelled surface
  (not separately charged or gated beyond service access — the
  III.F.3.a reading in §1, kept a launch-gate legal item), gated
  on: the §3 CSP security review,
  terms/privacy updates (III.A), and the 30-day TTL job. (The two
  owner decisions that gated this record closed 2026-11-03: §5, §7.)
  The CI guardrails (grep gate over the new video/curation paths —
  no `googlevideo`, `/youtubei/` or `yt-dlp` references there; the
  kept, flag-off `workers/youtube` spike directory is the one
  exclusion, per §7; no `search.list` on the play path) are
  acceptance criteria for the build changes, which also re-check
  the YouTube policy revision history — these policies change, and
  this ADR cites the revisions read above.
- Follow-on build work, each its own change: mirror schema
  extension and retirement of the superseded hourly-replication
  path (import runbook, packet ledger/evaluator) (§2, §5);
  `PlayableSource` + `YouTubeEmbedSource` + video
  surfaces (§1); curation job + moderated submissions (§2);
  ListenBrainz account link (§4); the BrainzPlayer study (REA
  angle, done 2026-11-03) as build input to the §1 build; and,
  only after embeds work, the §7 spike
  track behind its flag with its proxy A/B/C measurement — any
  proxy spend and any production flag-on each need their own owner
  decision first.
- Open risks carried, not closed: III.I.1/III.E.1 audit exposure
  (§1), the accepted owner policy/ToS risk on the kept spike (§7),
  unlicensed CAA art (§6), Wikidata P136 genre coverage and the
  NC-data legal question (§5 open questions). All are pre-launch
  items.
