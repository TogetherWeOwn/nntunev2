# Attribution inventory contract

This delivers attribution **data and validation**, not the in-app licences/about
page. Presentation, navigation, accessibility and final page copy remain separate
design and implementation work. There is no new route, UI, token, dependency,
network request, ingest behaviour or artwork cache.

## Integration

`apps/web/src/lib/attributions/index.ts` exports the JSON-serializable
`attributionInventory` (`schemaVersion: 1`), `activeAttributions`,
`plannedAttributions`, `fixtureAudioLicenceRecord`, and the pure
`validateAttributionInventory(unknown): string[]` validator. Types are exported
alongside the data. A future Svelte page can import from `$lib/attributions` and
render **activeAttributions**, not the whole inventory. Planned entries are
inactive provenance records, never evidence of current integrations. Schema
shape or meaning changes require a versioned contract update and consumer review.

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
  for product use by this inventory. ListenBrainz dumps and LRCLIB stay planned;
  LRCLIB's content rights remain unassessed, not inferred from server code terms.
- Referenced cover art stays explicitly rights-flagged, not openly licensed by
  assumption. This work authorizes no copying, caching or redistribution.
- The [fixture-audio licence record](fixture-audio-licences.md) and its per-album
  manifests remain authoritative and unchanged; this contract references rather
  than replaces them.

Follow [AGENTS.md — Clean-room, YouTube and MetaBrainz guardrails](../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails).
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
