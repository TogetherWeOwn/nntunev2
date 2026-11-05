# Video-link curation pipeline (N-videolinks)

The intended pipeline harvests candidate YouTube IDs, batch-validates them
ahead of time, refreshes or deletes API-returned data within 30 days, and
reports coverage weighted by played tracks. Playback must only use fresh,
validated IDs with the official video embed.

## Implementation status

`workers/mb/src/videolinks.rs` provides parsing and policy helpers.
`workers/mb/src/videolinks/validation.rs` adds a **bounded validation
runner** and an **app-side PostgreSQL store adapter**, not an activated
curation service. No route, schedule, database binding or credential-bound
HTTP adapter calls the runner yet.

`refresh_due` purges expired API-returned fields before contacting the
validation client, requests at most 200 distinct IDs in four 50-ID batches,
and stops on the first failure without retrying in the run. The whole run
has a 45-second deadline. Only HTTP 200 with an explicit items array can
be applied: malformed bodies, API error envelopes, duplicate/unrequested
IDs and oversized payloads fail closed. A successful empty array marks
requested IDs rejected; a failed request never does. Completed batches
survive a later failure. Validation timestamps precede the requests, so
network latency cannot extend the 30-day TTL.

`PgValidationStore` targets only the app-side tables. It clears expired
title/channel/duration/embeddability fields, selects due IDs (including
refresh one day before expiry), and commits each checked batch atomically
with a 1.5-second PostgreSQL statement timeout. Missing/private/non-embeddable
videos are rejected. An older or same-second validation cannot overwrite a
newer committed validation. Expiry cleanup retains the internal validation
timestamp for that comparison. The adapter must never receive `MB_DB`.

`migrations/0035_videolinks_curation.sql` still proposes the three app-side
tables; its manifest entry does not establish a staging apply. Runner
fixtures exercise orchestration and serialization. The feature-gated
`videolinks_postgres` acceptance target exercises the actual adapter and
unchanged migration against Actions' disposable PostgreSQL 18 service,
not staging or live YouTube. Its exact-head job result is required before
claiming SQL execution verified. Neither fixture target proves streaming
HTTP or a deployed TTL schedule. Without an active schedule and measured
cleanup throughput, the adapter alone does not establish metadata deletion
within 30 days.

### PostgreSQL acceptance boundary

The required `api-migrations` job creates a dedicated empty
`nntune_videolinks_ci` database and runs the explicit `postgres-tests`
feature / `videolinks_postgres` target. Missing configuration, a non-local
host/hostaddr, a different database name or existing curation tables fails
closed; no application credential is used. The tests apply/replay the real
migration and call `PgValidationStore`, rather than copying its SQL into a
second implementation. They cover shared IDs, rejected/missing videos,
metadata round trips, exact-boundary cleanup, refresh-ahead selection,
queue limits, stale/same-second writes, constraint-induced batch rollback,
row-lock statement timeouts and earlier-batch persistence after API failure.
The whole database contract has a 30-second deadline. This target does not
run in ordinary database-free MB tests; the MB Clippy lane includes it.

Only native test dependencies enable Tokio runtime/TCP features. Existing
Tokio/PostgreSQL versions remain pinned; the added Mio lock entry matches
the API worker's existing pin. Reviewed exact cached manifests for Tokio,
Tokio PostgreSQL, Mio and socket2 declare no build script, build dependency
or proc-macro. No new lifecycle-hook exemption is added. Cargo provides no
stable native release-age filter or blanket build-script/proc-macro disable;
these residual gaps remain. Locked dependency resolution, compilation and
test evidence come from isolated Actions jobs, not a local build. The host's
nested-worktree Cargo metadata refusal is not worked around by moving the
checkout or changing workspace membership.

Still required before feature acceptance:

- Wire source readers, the runner's app-side connection and a bounded
  schedule. Exercise the PostgreSQL adapter against the proposed schema.
- Implement the credential-bound HTTP adapter: fixed official HTTPS
  endpoint, no redirects, capped streaming body, cancellation on drop and
  sanitized failures. The runner's post-read body-size check is not a
  substitute for a transport-level allocation cap.
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
  fields, ignoring thumbnails. The store adapter persists only checked
  batch results, never the raw API response.
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
