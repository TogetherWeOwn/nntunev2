# Video-link curation pipeline (N-videolinks)

The intended pipeline harvests candidate YouTube IDs, batch-validates them
ahead of time, refreshes or deletes API-returned data within 30 days, and
reports coverage weighted by played tracks. Playback must only use fresh,
validated IDs with the official video embed.

## Implementation status

`workers/mb/src/videolinks.rs` currently provides **pure helpers**, not a
running curation job: URL/ID parsing, candidate deduplication, request
parameter construction, response parsing, freshness calculations,
moderation transitions and coverage arithmetic. The acceptance tests call
these helpers with synthetic inputs; they do not exercise a source import,
HTTP validation, persistent moderation, TTL sweep or coverage job.

`migrations/0035_videolinks_curation.sql` proposes the three app-side
tables. Its manifest entry does not establish a staging apply. No runtime
caller currently reads or writes these tables. Neither the schema nor a
freshness predicate alone enforces metadata deletion within 30 days.

Still required before feature acceptance:

- Wire source readers and an ahead-of-time batch-validation job to the
  app-side store, including failure handling and a bounded schedule.
- Connect authenticated submission/moderation and fresh-link reads.
- Implement and verify metadata refresh/deletion, not just stale-read
  rejection, within the 30-day limit.
- Query actual played-track counts and persist/report coverage snapshots.
- Verify the runtime credential binding without exposing the key, and
  record live validation separately from fixture-based verification.
- Obtain exact-head GitHub Actions evidence, independent review and the
  applicable migration/release approvals. No live result is claimed here.

## Sources (planned harvest wiring)

| Source | Origin | Licence note |
|---|---|---|
| `mb_url` | W6b mirror recording-URL relations (`mb_recording_url` video-marked rows + music-video pairs) | MusicBrainz core data, CC0 |
| `wikidata` | Wikidata P1651 (YouTube video ID) statements | CC0 |
| `listenbrainz` | Links observed in ListenBrainz data | CC0 dumps only |
| `user_submission` | Moderated user submissions (approved only) | App-side, never mirrored |

The extraction helper accepts watch, share (`youtu.be`), embed, shorts,
live and music-app URL forms and normalises them to 11-character video
IDs. Candidate deduplication keeps the highest-priority source (mirror
first, user submissions last). Callers must admit only approved
submissions; deduplication itself does not check moderation.

## Validation (`videos.list` batches)

- `validation_batches` chunks supplied IDs into groups of at most 50.
  The future caller must validate IDs and send each batch separately.
- `videos_list_query` builds parameters, not an HTTP request. No key is
  accepted or stored by the helper; the runtime caller must supply it
  securely. A credential approval is not proof of installation.
- The response parser keeps status, content details and snippet display
  fields, ignoring thumbnails. It does not persist them.
- `is_playable` checks public/unlisted and embeddable status. It does
  not enforce freshness; fresh-link readers must enforce both predicates.
- No playback-time search is permitted and no search client is added.

## TTL (30 days)

`is_fresh` and `expires_at` calculate the 30-day boundary.
The proposed schema carries `validated_at` / `expires_at`. Writers must
set those fields together with validation data. Readers must reject
`expires_at <= now()`; the refresh/delete job must also remove stale
API-returned fields. Only video IDs may persist long-term.

## Moderation queue

`normalise_submission` accepts a video ID or a YouTube URL.
`decide_submission` permits `pending` → `approved` / `rejected` and rejects
re-deciding terminal states. Persistent enforcement still needs an atomic
conditional write, moderator authorization and approval-only harvesting;
the enum transition alone is not a deployed moderation queue.

## Coverage report

`compute_coverage` calculates the raw share from a caller-supplied set of
fresh linked recordings and the **played-weighted** share from supplied
play counts. The played share is absent, not zero, when no play data
exists. Querying the catalogue and plays, filtering fresh playable links,
and writing `video_coverage_snapshots` remain integration work.

## Player-surface requirements (for the embed card)

These retain AGENTS.md **Clean-room, YouTube and MetaBrainz guardrails**:

- Visible official embed player only (at least 200×200, 480×270
  recommended), never covered or overlaid; controls, ads and branding
  untouched.
- At most one autoplaying player, only while more than half of it is in
  view; pause when the page or player hides.
- No audio-only, background, minimised or lock-screen playback; no
  caching or offline copies; thumbnails are never proxied or cached.
- The surface links the YouTube Terms of Service and the Google Privacy
  Policy, and works on the free plan.
