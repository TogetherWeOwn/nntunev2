# Attribution inventory contract

The versioned inventory below is consumed by the in-app licences/about section
(`About NNTune` at `/settings#about-nntune`, `apps/web/src/routes/settings/+page.svelte`).
That section adds no new route, token, component or dependency: headings, the
version line and the two licence intro lines follow the approved UX design,
and every visible notice and provenance URL renders verbatim
from this contract (link labels and the unavailable-error line are the only
functional page strings; obligations remain JSON-only compliance guidance). There is no new
dependency, network request, ingest behaviour or artwork cache.

## Integration

`apps/web/src/lib/attributions/index.ts` exports the JSON-serializable
`attributionInventory` (`schemaVersion: 1`), `activeAttributions`,
`plannedAttributions`, `fixtureAudioLicenceRecord`, and the pure
`validateAttributionInventory(unknown): string[]` validator. Types are exported
alongside the data. The Settings page imports from `#lib/attributions` and
renders **active** licence entries under `Licences` and active data entries
under `Data sources` (omitted while none are active). Planned (inactive)
entries and per-entry obligation guidance do not render visibly at all: the
accurate inactive treatment is absence, and obligations stay in the embedded
`nntune-attribution-inventory` JSON provenance block alongside the full
inventory. The page renders an announced error when validation fails.
Schema shape or meaning changes require a versioned contract update and consumer review.

Each entry has an ID, name, kind, activation state, licence identifier (or an
explicit unresolved-rights marker), authoritative source/licence URLs, our own
notice summary, obligations and repository evidence. Package entries also carry
the direct manifest name, declared specifier and locked version. Notice summaries
are not substitutes for full upstream licence/copyright notices in distributions.
This inventory is not a claim that all distribution obligations are discharged.

## Scope and provenance

- Direct web runtime scope: Svelte, SvelteKit and its Cloudflare adapter, including
  runtime frameworks declared as dev dependencies. The other direct web packages
  are explicitly classified as build/test tools in `WEB_TOOL_PACKAGES`.
- This is not a complete SBOM or a transitive, Rust, Worker, container or toolchain
  licence audit. Review those separately before claiming complete distribution
  compliance. No guessed icon vendor: the current stencils are original project
  work under the repository MIT licence.
- Self-hosted Figtree uses the existing full OFL notice and font provenance record;
  platform system fonts are not redistributed by the app.
- MusicBrainz is **core CC0 only**, including URL relationships. No supplementary
  NC data, replication packets, tags, genres, ratings or annotations are licensed
  for product use by this inventory. The MusicBrainz mirror has not completed
  import or activated product reads, so the core-data entry stays planned until
  a future integration PR verifies the consumed dataset. ListenBrainz dumps and
  LRCLIB stay planned too; LRCLIB's content rights remain unassessed, not
  inferred from server code terms. The unmounted synced-lyrics resolver returns
  the `lrclib` inventory ID for provider results; it installs no live transport
  and writes no lyrics to persistent storage. Synced lyrics stay off, even for
  private use, pending the separate licensing decision required by
  [ADR 0012](adr/0012-compliant-youtube-video-and-brainz-data.md#5-metabrainz-licence-correction--free-cc0-only-owner-decision-2026-11-03).
  Any future activation is for private use only, with no spend or public rollout,
  and must visibly credit and link LRCLIB through the existing attribution
  contract without claiming a content licence.
- Referenced cover art stays explicitly rights-flagged, not openly licensed by
  assumption, and planned alongside the mirror. This work authorizes no copying,
  caching or redistribution.
- The [fixture-audio licence record](fixture-audio-licences.md) and its per-album
  manifests remain authoritative and unchanged; this contract references rather
  than replaces them.

Follow [AGENTS.md — Clean-room, YouTube and MetaBrainz guardrails](../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails).

## Synced lyrics provenance and activation

The server bindings and the Now Playing panel are exercised with invented
text in separate unit tests. The panel accepts `{ trackId, result }` for the
current queue entry. The Library may reuse the playback ID across occurrences,
so that ID is only the presentation key. The existing system seek stays under
the native player. The Subsonic compatibility methods use artist and title
for their lyric lookup, and the mounted store supplies no text. See the
[Library entry contract](../workers/api/src/catalogue.rs),
[Now Playing panel](../apps/web/src/lib/player/LyricsPanel.svelte),
[system seek change](https://github.com/TogetherWeOwn/nntunev2/pull/1605) and
[Subsonic store](../workers/api/src/subsonic_dispatch.rs).

### Verified tags and sidecars

A future server reader must resolve the occurrence through the current
authenticated Library access rules and verify the published source bundle
before reading lyrics. `bindVerifiedLocalLyrics` compares the evidence's
library scope, playback ID, occurrence key and source revision with the
server's current selection. Use an in-process scope unique to the verified
access context, with nonblank identity strings of at most 256 UTF-16 code
units. Read provenance from the verified media or sidecar record. The
source revision pins both the media and any associated sidecar to immutable
versions. A matching filename, client request, MusicBrainz ID or display
metadata alone cannot establish that relationship. Use the existing
[storage and publication boundary](adr/0015-standard-s3-storage-boundary.md).

Capture an immutable selection at request start and compare it with the live
one at delivery. Clearing the selection or losing access retires it.
Changing occurrence or source revision, replacing the access context, or
returning to the same track uses a fresh object. A future response consumer
must also discard late replies before giving the panel lyrics, including
when the same playback ID is selected again. Only the playback ID, source
kind and parsed cues pass through the binding. Paths, revisions and access
scope stay server-side. The fixture binding takes already-verified evidence.
Source verification and the authorization check belong to the future reader.
See the [server binding](../apps/web/src/lib/server/lyrics.ts) and
[synthetic cases](../apps/web/src/lib/server/lyrics.test.ts).

Use a valid embedded tag first, then a verified `.lrc` sidecar in the same
namespace, directory and stem. Bound reads before acquiring text and use
the existing parser's 64 Ki UTF-16 code unit and 1,024 cue limits. Known
instrumentals, absent evidence and documents with no usable nonempty timed
cue return absence. These rules establish identity and provenance. Permission
to display the text is a separate prerequisite.

### Future LRCLIB fallback and rights decision

If authorized, use a confirmed cached-only exact lookup after verified local
lyrics are absent. Send only verified title, artist, album and duration,
with a 1–3,600 second duration for the upstream API. Require matching
metadata, at most two seconds of duration difference and nonempty synced
cues. Treat unresolved or ambiguous editions as absent. Recheck access,
occurrence, revision and selection at delivery. Keep any permitted cache in
memory, scoped to the authorized library and verified occurrence/revision.
The existing synthetic resolver gives an upper bound of 128 entries, one
hour for hits, five minutes for confirmed absence and two seconds for a
lookup. Transport errors, rate limits, malformed replies and timeouts stay
out of the miss cache. If activated, show and link the existing `lrclib`
credit wherever its lyrics appear. Apply upstream client identification,
throttling and retry guidance in any authorized adapter. [LRCLIB API documentation](https://lrclib.net/docs.md)

**Recommendation for the rights decision.** Keep real synced lyrics off,
including for private use, until the rights holder or qualified counsel
confirms the permissions for copying, displaying and serving lyrics from
embedded tags and sidecars. Ask LRCLIB to confirm its API terms, whether it
can authorize in-memory copying and private display of the lyrics it serves,
and how updates, withdrawals and takedowns must be handled. Confirm a
supported lookup that cannot trigger upstream acquisition. The current
public docs and published router list `/api/get` and omit `/api/get-cached`.
The docs describe possible background acquisition after a miss on `/api/get`.
Keep the fallback off until a supported
cached-only contract is established. [LRCLIB endpoint docs](https://lrclib.net/docs#get-lyrics-with-a-tracks-signature)
[Published router revision](https://github.com/tranxuanthang/lrclib/blob/ffdfed4dd8288f9065f5562508c13705cbe0436c/server/src/router.rs#L29-L63)

LRCLIB's MIT licence covers its server software. Its public API docs describe
API access and copyright flags but do not state a lyric-content licence.
The US Copyright Office identifies lyrics as part of the musical composition,
which has rights separate from the sound recording. Any later permission
must cover the intended private use, with no spend or public rollout under
the current product decision. [LRCLIB software licence](https://github.com/tranxuanthang/lrclib/blob/ffdfed4dd8288f9065f5562508c13705cbe0436c/LICENSE)
[LRCLIB reporting docs](https://lrclib.net/docs#flag-a-lyrics)
[US Copyright Office Circular 56A](https://www.copyright.gov/circs/circ56a.pdf)
[ADR 0012](adr/0012-compliant-youtube-video-and-brainz-data.md#5-metabrainz-licence-correction--free-cc0-only-owner-decision-2026-11-03)
REA angle: reuse our shipped manifests, font licence record and original icon
provenance; no external product analysis or assets are needed.

## Validation and upkeep

The existing GitHub Actions `ci` workflow's `vitest` job discovers
`apps/web/src/lib/attributions/attributions.test.ts`; no new gate or script is
needed. Tests reject missing/empty fields, invalid URLs, unknown states, malformed
records and duplicate identities. Drift checks compare **all direct package
names** against runtime/tool classifications and runtime specifiers/versions
against both `package.json` and the pnpm v9 root importer. Add/remove/rename a
direct dependency or update a runtime version: review classification, authoritative
licence terms, notices and inventory pins in the same PR. Tool version changes
and transitive version changes are outside this scoped drift check. Unsupported
lock layouts fail closed for the runtime pins rather than silently passing.

Before activating a planned data source, review its actual consumed dataset,
rights and obligations and change its evidence/status in the integration PR.
Do not activate a source simply because its API or software is available.
Full suites, type checks and builds run on Actions, never as host verification.
