# R1–R4 feature-parity programme (approved 2026-11-03)

Status: approved programme order. The full ranked catalogue lives in
the approved programme document of 2026-11-03 (version 1.4); this page
records the order, the launch scope and the current status view with
dated evidence. Release order: R1 shell + core playback, R2 catalogue
screens, R3 discovery + video, R4 social + cross-device.

All releases play the shared curated catalogue under the launch scope
in [ROADMAP.md](../../ROADMAP.md): lossless audio first, official
video embed second, otherwise hidden; one concurrent stream per user;
private playlists only; invite-only paywall in payment test mode for
staging. Prices are launch proposals; nothing here activates live
billing.

## Release summaries

- **R1 — shell + core playback.** Three-pane shell with mobile
  tab/mini-player, real player bar (transport, seek, volume), queue as
  a panel, server-backed library with liked songs and recently played,
  shell furniture (toasts, empty/loading/error states, context
  menus), keyboard and screen-reader parity. Player helpers for
  MediaSession, saved volume, sleep timer, speed and resume graduate
  from the demo; production audio leveling is deferred (DEF-L).
- **R2 — catalogue screens.** Unified search with suggestions and
  availability states, album and artist pages, private playlists
  (create/rename/reorder/add-from-everywhere), save albums and follow
  artists, gapless and crossfade, drag-and-drop with keyboard
  equivalents, full track credits from public relations data.
  Availability states: lossless-available, video-only, unavailable
  (hidden by default; an admin setting may show unavailable tracks
  greyed out with a placeholder for a future request action).
- **R3 — discovery + video.** Optional listening-history integration
  (account link, shelves, radio, similar artists), song-relations
  view, music-video tab with the official embed, video-link curation,
  free-source lyrics. Every personalised surface degrades to a useful
  non-personalised one when history or linked data is absent. Lyrics
  come from embedded tags, sidecar files and a free lyrics service,
  cached and credited; the tab hides when nothing is found.
- **R4 — social / cross-device.** Cross-device playback state with
  handoff (moves the single stream, never multiplies it), then device
  picker and remote control. Privacy defaults surface before anything
  social ships. Profiles, follows, activity, collaborative playlists
  and taste blends are later possibilities requiring a separate
  sharing decision; they do not change the private-playlist boundary.

## Explicitly removed or later

- No offline downloads. No user provider setup, user credentials, or
  user archive intake — backend providers only.
- No public playlists or share links at launch. No transcode/quality
  picker or data-saver stream at launch. No install prompts at launch.
- Verified rendition quality details, production leveling and
  speculative next-track prefetch are deferred (accepted 2026-11-04),
  not delivered. Current-source buffering is not next-track prefetch.
- Genre browsing depends on licence-compatible data and adequate
  coverage of the playable catalogue. Mood labels are not assumed to
  have the same foundation. Genre stays in the later conditional set
  until the genre build lands and coverage measures at least 80
  percent on the real ingested library.

## Status view (2026-11-10)

| Item | State |
|---|---|
| Programme order R1 → R2 → R3 → R4 | approved 2026-11-03, unchanged |
| Launch scope (lossless-only, 1 stream, private playlists, no downloads) | approved 2026-11-03, in force |
| M1 exit journey | approved 2026-11-03; implemented in part; verification pending (see [handoff](../handoff.md)) |
| R1/R2/R3/R4 build items | approved scope; implementation per-item — do not infer completion from this page; check the traceability matrix and exact-head Actions evidence for what is verified |
| Production launch | proposed; gated on M1 exit, billing/player gates and release validation |

Verification route: exact-head GitHub Actions evidence
(`ci-ok` gate plus the deploy and staging evidence described in
[docs/runbooks/](../runbooks/README.md)). Do not claim verified from a
merged change alone.
