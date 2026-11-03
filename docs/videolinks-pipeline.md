# Video-link curation pipeline (N-videolinks)

Harvest candidate YouTube IDs, batch-validate them ahead of time, expire
API-returned data after 30 days, and report coverage weighted by played
tracks. At ~1.6% raw MusicBrainz coverage, this pipeline *is* the video
feature: play time only ever serves already-validated IDs to the embed.

## Sources (harvest)

| Source | Origin | Licence note |
|---|---|---|
| `mb_url` | W6b mirror recording-URL relations (`mb_recording_url` video-marked rows + music-video pairs) | MusicBrainz core data, CC0 |
| `wikidata` | Wikidata P1651 (YouTube video ID) statements | CC0 |
| `listenbrainz` | Links observed in ListenBrainz data | CC0 dumps only |
| `user_submission` | Moderated user submissions (approved only) | App-side, never mirrored |

Harvest accepts watch, share (`youtu.be`), embed, shorts, live and
music-app URL forms and normalises them to 11-character video IDs.
Cross-source duplicates collapse to the highest-priority source
(mirror first, user submissions last — submissions still need approval
plus validation). Logic: `workers/mb/src/videolinks.rs`.

## Validation (`videos.list` batches)

- IDs are validated in batches of at most 50 **ahead of time**, never
  at play time. Play time never calls `search.list`.
- Only the parts needed for validation are requested (`status`,
  `contentDetails`, `snippet` for the moderation display).
- A video is servable when it is public (or unlisted) **and**
  embeddable. Anything else stays in the candidates table as rejected.
- The API key is runtime config (scoped credential, operator step) and
  is appended at the edge; it is never stored in code or the database.

## TTL (30 days)

Every API-returned field carries `validated_at` / `expires_at`
(`validated_at` + 30 days). Readers treat `expires_at <= now()` as
stale: refresh via `videos.list` or delete, never serve. Only video
IDs are stored long-term; titles and channel names are display metadata
under the same TTL.

## Moderation queue

User submissions accept a video ID or a YouTube URL. States are
`pending` → `approved` / `rejected`; decided submissions are terminal
and cannot be re-decided. Approval admits the ID as a
`user_submission` candidate — it is still validated before it serves.

## Coverage report

`compute_coverage` reports the raw share of recordings with at least
one fresh validated link, plus the **played-weighted** share (plays of
linked recordings over all plays). The played share is absent — not
zero — until N-plays data exists. Snapshots persist to
`video_coverage_snapshots` (`migrations/0031_videolinks_curation.sql`).

## Player-surface requirements (for the embed card)

- Visible official embed player only (at least 200×200, 480×270
  recommended), never covered or overlaid; controls, ads and branding
  untouched.
- At most one autoplaying player, only while more than half of it is in
  view; pause when the page or player hides.
- No audio-only, background, minimised or lock-screen playback; no
  caching or offline copies; thumbnails are never proxied or cached.
- The surface links the YouTube Terms of Service and the Google Privacy
  Policy, and works on the free plan.
