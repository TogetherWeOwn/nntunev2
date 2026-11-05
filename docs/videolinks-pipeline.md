# Video-link curation pipeline (N-videolinks)

The intended pipeline harvests candidate YouTube IDs, batch-validates them
ahead of time, refreshes or deletes API-returned data within 30 days, and
reports coverage weighted by played tracks. Playback must only use fresh,
validated IDs with the official video embed.

## Implementation status

`workers/mb/src/videolinks.rs` provides parsing and policy helpers.
`workers/mb/src/videolinks/validation.rs` adds a **bounded validation
runner** and an **app-side PostgreSQL store adapter**, not an activated
curation service. `validation/http.rs` supplies an inactive Worker HTTP
client requiring a runtime Secret. `storage.rs` supplies bounded external
candidate admission and fresh-link database reads. `harvest.rs` supplies a
read-only paged MusicBrainz mirror source and a bounded runner that admits
its pages into the app store. `moderation.rs` supplies a persistent pending
queue and atomic decision/promotion in the app database. No route, schedule
or database binding calls these adapters yet.

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
not staging or live YouTube. Both database test functions passed at
`512590d06ac06082e596b7b4ed56327ca5825d62` in the
[Actions migration job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42002762144/job/127161854863).
That verifies SQL execution at that head, not streaming HTTP or a deployed
TTL schedule. Without an active schedule and measured cleanup throughput,
the adapter alone does not establish metadata deletion within 30 days.

### Worker HTTP adapter boundary

`WorkerValidationClient` constructs only the official HTTPS `videos.list`
request, refuses redirects, validates 1–50 distinct IDs before networking,
and projects only ID, status, duration, title and channel fields (no
thumbnails). It accepts an existing runtime Secret, never a candidate-supplied
URL or key. The key-bearing type has no Debug/Display implementation; all
transport, header, body and credential failures map to sanitized errors.
Non-200 responses are rejected without reading API error bodies; no request
is retried. No Cache API or response persistence is used.

A 10-second request deadline includes headers and body, within the runner's
45-second total. The abort controller stays alive through body consumption
and aborts on success, early failure, caller cancellation or timeout. A JS
reader checks each chunk's byte length **before** copying it into Wasm;
the retained buffer cannot exceed 256 KiB, even with no Content-Length or
one oversized chunk. The browser/runtime still owns incoming JS buffers;
this is not a bound on its network allocations. Invalid UTF-8 fails closed.

Five native unit tests cover request construction, invalid inputs, body
budget boundaries, split UTF-8 and cancellation-guard drop. They do not
exercise actual Worker fetch/AbortSignal behaviour. The
[MB Rust job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42009295264/job/127183903094)
passed at `52f88ff60d7596cacaadf4af8e282cd843f78acc`, including its native
test and wasm32 compilation steps. That is not live transport evidence;
an installed secret and live transport validation remain unproven. No
dependency, binding, route, schedule or migration changes accompany this
adapter.

### Candidate admission and fresh-read boundary

`PgCandidateStore` is a second inactive app-side adapter over migration
0035. `admit_external_candidates` accepts at most 200 recording/video
pairs, requires canonical UUID-shaped recording MBIDs and valid video IDs,
and deduplicates after lowercasing MBIDs. Source priority is preserved
across repeated/concurrent admission: an existing row's provenance can
upgrade, but validation data, TTL and first-seen time are not reset. A
new association starts pending even when the same video is already
validated for another recording. The whole write is atomic.

This method trusts the caller to establish external source provenance
and licence eligibility; it is **not a user-submission endpoint**. It
refuses `user_submission` inputs instead of trusting an approval flag.
Persistent approval and candidate promotion use the separate atomic
`PgSubmissionStore` described below; moderator authorization remains the
future route's responsibility. Source readers are not wired by this adapter.

`fresh_links` accepts at most 50 recording MBIDs and returns at most five
links per distinct recording, ordered by source priority, validation time
and video ID. SQL filters out pending/rejected/expired, non-embeddable,
never-validated and future-dated rows before fetching metadata. Both
`expires_at > now` and validation age strictly below 30 days are required;
an overlong stored expiry cannot extend TTL. The clock is supplied by the
trusted caller, not an end user. Each admission/read statement has a
1.5-second server timeout. Reads perform no network request, do not cache
metadata and do not replace the required scheduled cleanup.

Four native input/serialization fixtures and expanded disposable-PostgreSQL
acceptance cover database-specific source labels, canonical deduplication,
replay/priority, metadata
preservation, no partial admission on invalid input or constraint failure,
new associations, freshness boundaries and the 50-by-five read cap.
The [MB Rust](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42012759761/job/127195646775)
and [PostgreSQL](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42012759761/job/127195646761)
jobs passed at `51142ac46a9350dd1d2df9793a87ca2e1f3c1a9e`. This is not a
live database apply, authenticated route or deployed cleanup claim.

### MusicBrainz mirror source boundary

`PgMirrorSource::candidate_page` is an inactive reader for the CC0-only W6b
mirror, separate from the app-side writer. Each call accepts at most 50
UUID-shaped recording MBIDs, canonicalizes and deduplicates them, and reads
at most 200 distinct raw recording/URL edges plus one lookahead. It follows
video-marked direct URLs and both directions of music-video pairs, always
associating the extracted ID with the **requested** recording. Multiple
relationship labels and duplicate paths do not consume extra page slots.
Non-video relationships are excluded in SQL; malformed IDs and non-YouTube
hosts are skipped. SQL returns no URL above 2048 bytes. Raw source URLs are
not persisted or exposed by the page result; candidates carry only MBID,
video ID and source.

The cursor is the last consumed **raw edge**, even when a page has zero
candidates. The lookahead is not consumed. This prevents an invalid-URL
page from hiding later usable links. Admission can deduplicate the same ID
across pages; the reader does not write candidates or validate videos. A
cursor is valid only for an unchanged recording selection and mirror
snapshot. Calls use separate read-only transactions, not a snapshot across
pages: the scheduler must restart a scan after an import, retaining
idempotent app-side admission. This is not a complete catalogue scheduler.

The reader uses explicit public table names, bound parameters, a read-only
transaction and a 1.5-second statement timeout. Bounded results do not imply
constant query work over the source graph. No live MusicBrainz API, other
source API, YouTube request, app write, credential, schema, binding, route
or schedule change is introduced.

Three native fixtures cover selection/cursor policy, invalid-page progress,
lookahead, deduplication and URL bounds. The existing disposable PostgreSQL
contract adds actual reader SQL against minimal W6b reader-column fixtures
(not a full mirror migration/apply), symmetric pairing, non-video exclusion,
pagination/replay across recordings and lock timeout/recovery. The
[MB Rust](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42015850006/job/127206647588)
and [PostgreSQL](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42015850006/job/127206647282)
jobs passed at `1a0122a0fdabb4b4eee9fc134eff19061009df74`. This is reader
fixture evidence, not a full mirror apply or live source verification.

### Mirror-to-app admission boundary

`admit_mirror_pages` connects the two adapters when explicitly called with
separate mirror/app connections. One invocation reads at most four pages
(800 raw edges plus page lookaheads), under a 20-second total deadline.
Each page's candidates are admitted atomically; an empty candidate page
still advances on its raw edges. It never calls YouTube or validates an ID.

Reports carry acknowledged page/raw-edge/candidate counts, changed app rows,
a resume cursor and an explicit completion flag. Candidate counts are
page-local sums, not whole-scan unique counts or coverage. The resume cursor
advances **only after admission succeeds**. A source/write failure or
deadline returns earlier progress and does not retry in the invocation;
earlier commits survive. A failed/cancelled commit can have an unknown
outcome, so replay is deliberately idempotent rather than exactly-once.
Dropping the waiter is not a guarantee of server-side cancellation; the
adapters retain their 1.5-second statement timeouts.

The caller owns persisting the checkpoint and retaining the same recording
selection and unchanged mirror snapshot; separate page transactions do not
establish a snapshot. Restart idempotently after an import. There is no
runtime binding, schedule, durable scan queue or automatic restart policy
yet, and the runner is not activated by adding it to the module. No schema,
dependency, workflow, credential, route or deployment changes accompany it.

A native deadline fixture checks cancellation and retained progress. The
expanded disposable-PostgreSQL contract covers four-page bounds plus a
final page, duplicate relations and symmetric pairs across page boundaries,
an invalid-only first page, idempotent replay, source timeout and a later
atomic admission failure with earlier commits retained and cursor-based
recovery. The two test connections share the disposable database only to
supply both fixtures; that is not evidence of deployed connection isolation.
The [MB Rust](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42018801274/job/127216082156)
and [PostgreSQL](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42018801274/job/127216081546)
jobs passed at `7d953371d712507907cabbb68d2a0cb09ff3cda1`. Web E2E was
still running when recorded; no aggregate or live verification is claimed.

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

- Wire the mirror admission runner, remaining source readers, app-side
  connections and a bounded schedule with durable checkpoints. Keep the real
  PostgreSQL acceptance target green at each head.
- Exercise the inactive HTTP adapter in the Worker runtime: fixed official
  HTTPS endpoint, no redirects, capped streaming body, cancellation on drop
  and sanitized failures. Native policy fixtures do not prove live fetch or
  AbortSignal behaviour.
- Connect authenticated submission/moderation and wire fresh-link routes to
  the bounded app-side reader. Admission/reads alone do not authenticate callers.
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

`PgSubmissionStore` is an inactive adapter over migration 0035's app-side
submission table. `submit` accepts one canonical recording MBID, an ID or
recognized URL (at most 2048 input bytes), and a trusted session-derived
submitter identifier (1–128 bytes, non-blank, no control characters). It
stores only the normalized ID, never the raw URL or API metadata. Duplicate
submissions remain separate pending entries; this primitive does not provide
request idempotency, abuse controls, recording existence checks or quotas.

`pending` returns at most 50 oldest pending rows, with a deterministic ID
tie-break. No API display metadata is fetched. It is a moderator worklist,
not a public feed. `decide` conditionally updates only a pending row, records
a database decision time and optional single-line reason (1–1024 bytes),
and promotes an approval to the candidate table in the **same transaction**.
Rejection never promotes. A new candidate starts pending validation; approval
is not evidence that an ID is playable. Conflicts preserve an existing
candidate's stronger provenance, metadata, validation, TTL and timestamps.

Competing decisions have at most one winner. Missing and terminal rows both
return `NotPending`, and terminal decisions cannot be changed through this
adapter. Failed promotion rolls back the decision. Each SQL statement uses
a 1.5-second timeout; there is no in-call retry. A lost commit acknowledgement
still has an unknown outcome: a replay's `NotPending` cannot identify which
moderator/decision won. The current schema records submitter, reason and
decision time, **not moderator identity**; it is not a full moderation audit
log or a database-wide prohibition on other writers changing terminal rows.

Callers must authenticate the submitter, authorize every queue read and
decision, and enforce request/rate/duplicate limits before using this store.
No authenticated route or runtime binding is added here. Two native input
fixtures plus the expanded disposable-PostgreSQL contract cover normalization,
pending exclusion, approval/rejection, terminal replay, existing-candidate
preservation, constraint-induced rollback, competing decisions, lock timeout
and worklist bounds/progression. Execution of this increment awaits exact-head
Actions; no local tests/builds or deployed moderation acceptance are claimed.

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
