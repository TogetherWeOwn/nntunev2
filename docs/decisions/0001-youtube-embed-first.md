# Decision 0001 — YouTube: embed first (2026-11-03)

Date: 2026-11-03. Provenance: owner spending decision recorded in the
approved programme document (version 1.4) and architecture decision
record 0012.

## Context

The product presents a video feature for tracks without a suitable
lossless source and for music videos. A pre-existing extraction spike
explores downloading the underlying media stream. The compliant
shipping path under the video platform's terms is the official embed
player.

## Decision

Build the visible official embed only. The extraction spike is not
retired but is not built on: it sits behind a flag, off in production,
as a post-discovery-phase research track. No release item may depend
on extracted audio. A residential proxy is not approved. Any future
proxy spend requires its own approval with a monthly cap; nothing
pre-approves it.

Embed rules: visible official player only, never covered or overlaid;
platform controls and branding stay untouched; at most one autoplaying
player, only while more than half in view; playback pauses when the
page or player is hidden; no audio-only, background, hidden or
minimised playback; no caching or offline copies; never sold or gated
as a separate paid extra; no play-time search calls — video IDs are
resolved ahead of time and validated in batches; the platform terms
and privacy policy links appear wherever the embed is offered.

## Why

The embed is the only compliant playback path. The spike carries
platform-policy risk as an accepted owner risk behind its gates; it
does not make extraction compliant. Follow-on research starts only
after embeds work, with proxy order: none, then authenticated only,
then cheaper options.

See [AGENTS.md](../../AGENTS.md#youtube-embed-only) for the full rule
set and [docs/adr/0012-compliant-youtube-video-and-brainz-data.md](../adr/0012-compliant-youtube-video-and-brainz-data.md).
