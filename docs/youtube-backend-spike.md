# YouTube backend spike: yt-dlp-style extraction on Workers

Status: **Day 1 in progress** — non-proxy core landed (`workers/youtube`
crate + 100-video corpus); gate measurement pending staging deploy, and
the proxy-client slice parked until proxy credentials exist (see Open
dependencies). Nothing below is a gate result yet.

## Decision

Owner 2026-10-21: build our own yt-dlp-style YouTube backend on
Cloudflare Workers, with a residential proxy only for auth/extraction
if needed — not for streaming bytes.

Owner 2026-10-21 update (scope + proxy rules for this spike):

- The owner's home residential IP (gluetun `mcp-resi-vpn`) may be used
  ONLY for development/internal testing in this spike, never for
  anything public-facing. Production must use a separate residential
  proxy service. No home endpoint, credential or IP literal is committed
  anywhere; spike dev tooling reads it from local-only config.
- Scope is official music videos only (externally curated/verified);
  for audio, prefer the official track at the best available quality.
  The host's official-media selection research (2026-10-21) replaced
  the VEVO/`- Topic` standing proxy — see Official-media selection.
  Note: the host cited `ops/analysis/official-media-20261021/REPORT.md`
  but that path is not in the repo at this head; the rules below are
  implemented from the research comment text and are reconciled with
  the report when it lands.

## GO/NO-GO gate

Are label-video stream URLs bound to the extraction IP? The Day-1
measurement extracts 100 videos (50 label music, 50 audio tracks),
records each player `status` and the stream URLs' `ip=` value, then
fetches the first 256 KiB, a centered mid-file 256 KiB range and the
full file **from Worker egress**. If more than 5% of those byte fetches
fail, bytes would need the proxy too, which is uneconomic: NO-GO.

The verdict function is `evaluate_gate` in `workers/youtube/src/gate.rs`
(rate `<= 0.05` is GO; zero fetches fail closed). Secondary metric:
extraction-OK rate per client/IP, because finding P1 shows extraction
itself is egress-reputation sensitive.

## Official-media selection (host research 2026-10-21)

Key each recording by MBID + ISRC. Resolve the ISRC first to the
canonical artist, title and duration. Search YouTube Music `songs` and
`videos`, keeping only `musicVideoType` **OMV** (official music video)
and **ATV** (the official audio track), read from
`watchEndpointMusicConfig.musicVideoType`:

- Gate by a `channelId` allowlist plus the official-artist/verified
  badges. NEVER "Vevo"/"Topic" substring matching — the Day-1
  `day1.json` corpus was built on that proxy and is superseded pending
  a re-cut (see `workers/youtube/corpus/SOURCES.md`); `build.sh` fails
  closed until it is rewritten to this method.
- Prefer the ATV's `/next` **counterpart** as the OMV — YouTube's own
  same-recording link — after OMV re-verification. Otherwise take the
  top OMV passing duration (≈±5 s vs the canonical duration) and
  denylist checks.
- MusicBrainz video-recording links and Wikidata P1651 count only after
  a liveness check plus OMV re-verification. IMVDb (key required, beta)
  is a candidate source and is always re-verified.
- Audio: the exact-recording ATV at the best available Opus, discovered
  via the extractor (expect itag 251 for non-premium). Never take audio
  from the OMV. YouTube never beats NNTP FLAC; use it only where FLAC
  is missing.

The pure decision logic lives in `workers/youtube/src/select.rs`
(type parse, allowlist/badge/duration/denylist gate, counterpart
preference, ATV-only Opus choice, cache keys). The YTM search + `/next`
I/O lands with the runtime-entry slice; the corpus re-cut runs from
egress that is not bot-walled (Worker egress or the resi dev path).

Caching: MBID→selection about 30 days (`yt:sel:{mbid}:{isrc}`),
videoId→metadata about 7 days (`yt:meta:{video_id}`). Revalidate on
MusicBrainz edits or dead IDs. Measure the hit and escape rate per
layer before tightening the gates.

## Client: VISIONOS first

Host research (2026-10-21, live tests): the `VISIONOS` client returns
plain non-SABR URLs with no PO token and no `n` parameter; Range
requests give 206 from the test IP. The profile in
`workers/youtube/src/client.rs` is pinned to the upstream `youtubei.js`
client table (verified 2026-10-21 against upstream `main`):

- name `VISIONOS`, version `1.02`, client id `101`, platform `MOBILE`
- device `Apple` / `RealityDevice17,1`, OS `visionOS` / `26.5.23O471`
- Safari user agent from the upstream table
- API key: runtime config (KV), extracted from YouTube's public
  homepage JS at deploy time. Profiles carry no key material and no
  key-shaped literal is committed anywhere; the gitleaks gate enforces
  this.

Invariants to confirm against live `/player` responses during the
Day-1 measurement: zero `signatureCipher` formats, zero `n` params,
zero SABR URLs (the crate's SABR check is a documented heuristic:
`/sabr/` path or `sabr=1` param).

Client-table maintenance is expected monthly; upstream `youtubei.js`
is the reference for version bumps.

## Measured findings

- **P1 (2026-10-21, dev egress): extraction-IP reputation varies.**
  From dev-box egress, `VISIONOS`, `WEB` and `ANDROID` all return
  `LOGIN_REQUIRED` ("Sign in to confirm you're not a bot") on both
  Innertube hosts, with and without real visitor data — even for a
  non-label video (Blender's Big Buck Bunny). The host's test IP saw
  `OK` for non-label and `LOGIN_REQUIRED` only for label music videos.
  Consequence: the corpus measurement **must** run from Worker egress,
  and the proxy may be needed for extraction more broadly than
  "label-only", depending on Worker-egress reputation. The Day-1
  readout records the per-class extraction-OK rate from staging.
- **P2 (2026-10-21): VISIONOS profile verified.** Exact values above
  match upstream `youtubei.js` `main`; the earlier `LOGIN_REQUIRED`
  responses are IP reputation, not a malformed request (request shape
  matches upstream's session/headers behavior).

## Target architecture (to validate)

- KV-cached player responses (key: client + video ID; TTL under the
  measured URL lifetime).
- Client try-order kept in KV config (compiled default: VISIONOS only).
- Range passthrough on bytes (206 + seek from the extracted URLs).
- A Durable Object for rate limits and circuit breakers on extraction.
- 403 handling: re-extract once, then mark the video IP-locked (TTL'd
  per-video flag; IP-locked videos skip byte fetch and surface an
  honest error).
- Proxy (when needed): HTTP CONNECT via `connect()` + `startTls`, for
  `/player` only (~25 KB/extraction, ~$0.10/1k at ~$4/GB). PO-token
  (BotGuard) cases stay out of the Worker: Browser Rendering as an
  off-by-default fallback. Spike dev/internal testing may use the home
  residential IP; production uses a separate residential proxy service
  (procurement outside this spike) — the home IP never appears in a
  public-facing path.

## Day plan

- **Day 1** (this card's first slices): Worker + `connect()` proxy
  client; 100-video corpus measurement from Worker egress; non-proxy
  parts first. Done so far: portable extraction core, probe planner,
  gate verdict, verified corpus. Remaining: runtime entry, proxy
  client (needs creds), staging measurement run.
- **Day 2:** tv client with the QuickJS solver (meriyah + QuickJS
  compiled to WASM, no eval; `youtubei.js` ships a `cf-worker` entry
  point needing a custom evaluator). CPU time + memory, cold and warm;
  DASH manifest playback and seek in a browser.
- **Day 3:** 6 h soak of 1,000 extractions. Bot-wall rate, proxy GB,
  URL lifetime, CPU time.

## Open dependencies

- **Residential egress for spike dev testing (owner 2026-10-21):**
  the home residential IP via gluetun (`mcp-resi-vpn`), dev/internal
  only. The `connect()` proxy-client slice starts when the dev path to
  that endpoint is available (host/operator provides how to reach it
  from the dev environment); nothing proxy-dependent runs before that.
  No separate proxy spend is needed inside this spike; production's
  separate residential service is procured outside it.
- **Staging deploy** of the measurement worker (needs the runtime-entry
  slice first). No gate numbers exist until bytes are fetched from
  Worker egress.
