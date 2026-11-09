# Offline genre mirror preparation

Consumes the existing [CC0 interchange](../genre-extract/README.md), not a dump,
network service or MusicBrainz genre assignment. Python 3.11+ stdlib only. The
resolver traverses P279 only, supports multiple roots and curated overrides,
most-specific subsumption, cycle diagnostics, barriers and bounded traversal.
`taxonomy-v1.json` records our Product-owned stable IDs, display names, CC0
Wikidata anchors and two curated additions. No competitor taxonomy was used.

Follow [AGENTS.md “Clean-room, YouTube and MetaBrainz guardrails”](../../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails).
No supplementary MusicBrainz tags/genres, NC data, MLHD+, cover-art changes,
YouTube changes or fallback ingest policy are included.

## Preparation, not execution

Run from the trusted source tree, never an input directory:

```sh
python3 -I containers/genre-mirror/materialize.py \
  --extract /path/to/complete-local-projection \
  --config /path/to/reviewed-config.json \
  --cohort /path/to/local-artist-export.json \
  --output /path/to/new-materialization
```

This command does **not** connect to a database or run SQL. No full production
extract, actual library export, live query or refresh job is commissioned.
Inputs must be immutable during the operation. It is trusted offline filesystem
tooling, not a concurrent multi-user filesystem security boundary.

DDL remains a **non-executable design**, outside this source tree, until the
actual integration source contains the complete immutable predecessor history.
This source does not install a schema, append a migration/manifest entry or
prove database behavior. Existing reservations and ordered prefix integration
must be reconciled before adding the genuine migration and its disposable
Actions replay/rollback tests through the normal review/apply gates. Never copy
predecessor migrations, skip slots or use an alternate schema to evade them.

### Input contracts and bounds

- Require extract schema/version v1, declared Wikidata CC0 source, complete
  manifest, exact per-file SHA-256/bytes/row counts, unique keys and valid
  references. Duplicate JSON keys, malformed UTF-8/JSON, non-finite numbers,
  noncanonical QIDs/MBIDs and invalid/control-bearing labels refuse.
- Aggregate extract input at most 32 MiB; at most 10,000 genres and 50,000 rows
  per table. Config/manifest each at most 1 MiB. Cohort at most 32 MiB/50,000
  artist IDs per array. Aggregate output at most 32 MiB. These deliberately
  bounded source-integration limits do not claim production-scale throughput.
- Resolver memory retains the graph and **one** bounded closure, not a cache
  of every genre's ancestry. Config allows at most 64 roots/64 anchors each,
  1,024 barriers/10,000 overrides. Depth is 1–64, nodes 1–100,000 per traversal;
  v1 selects 32/10,000. CPU depends on graph fan-out and size; large future jobs
  require separately admitted external wall-time/RSS/disk limits.
- Root IDs are canonical lowercase ASCII `[a-z0-9_]{1,32}`. Non-retired roots
  require at least one present anchor. Duplicate roots, anchors or sort orders,
  unknown/retired override roots, conflicting actions or missing reasons refuse
  before publication. A missing override target is diagnostic only.
- Missing entities are leaves. Barriers neither contribute roots nor expand.
  Cycles terminate; directed-cycle detection does not mislabel a diamond.
  Capping retains roots already found and records `traversal_capped`.
- Empty results distinguish `excluded`, `unknown_genre`, `no_parents` and
  `no_root_reached`. More than the configured root threshold records
  `broad_multi` without choosing a winner. `replace`/`exclude` bypass traversal;
  `add`/`remove` run after most-specific subsumption.

The extractor's **normal + preferred statement-membership** semantics remain
unchanged; rank is not present in interchange v1 and cannot be reconstructed.
This source preparation must not be described as truthy/best-rank extraction.
Product v1 accepts this union for P279 and P136/P434 admission, excluding
deprecated/non-value claims and ignoring qualifiers/references. Both normal and
preferred rows remain when they coexist; a future best-rank requirement needs
a new producer contract, never consumer inference. P31 is never traversed.

### Cohort and coverage adapter

The local cohort is exactly:

```json
{
  "schema": "nntune.genre-artists.v1",
  "dataset_kind": "synthetic",
  "artist_mbids": ["11111111-1111-4111-8111-111111111111"],
  "playable_artist_mbids": ["11111111-1111-4111-8111-111111111111"]
}
```

`artist_mbids` is the CC0 **artist** mirror identity export; playable IDs must
be a subset. A syntactically valid P434 is not proof of an artist: the SQL load
also joins `public.mb_artist`, skipping unmatched IDs. The manifest lists IDs
missing from the supplied local artist export. No live export is performed.
The supplied cohort and installed mirror must describe the same snapshot before
real measurement. Coverage JSON contains only mirrored artists' resolved roots.

`coverage.json` conforms to the existing [coverage v1 contract](../../docs/genre-root-coverage.md).
That contract accepts hyphens, not underscores. The adapter maps canonical
`hip_hop` to transport `hip-hop` (likewise other underscore IDs), validates
collision-free slugs across all active **and retired** reserved IDs, and rejects
invalid/unmapped membership references before filtering artists. Only active
roots enter the resolved set; null memberships stay null. The separate manifest
records adapter `nntune.genre-coverage-adapter.v1` and the full forward/reverse
map, bound to config/taxonomy, validated producer manifest, cohort identity and
coverage input/report digests. No metadata is added to the strict v1 payload.
It never renames a Product ID or changes the calculator. `dataset_kind` is a caller
assertion, not attestation; `real_data_gate` remains `not_evaluated` even for
`actual-export`. Fixtures/popularity never establish >=80% actual-playable-artist
coverage. Later real measurement needs complete same-snapshot actual N-ingest
artists and reviewed root memberships. Below that gate retain curated shelves;
play weighting needs actual play data and is not implemented here.

## Outputs and refresh integrity

A new output directory holds `refresh.sql`, `genre_rollup.jsonl`, diagnostics,
coverage input, `coverage-report.json` from the existing calculator, the original
extract manifest and a final `manifest.json` provenance sidecar/completion marker.
The marker binds config bytes, taxonomy version, the complete validated producer
manifest (schema/version, extractor-source SHA, source/licence/dump, input SHA and
row digests), cohort and output digests, without timestamps or host paths. Hashes
prove consistency, not origin authenticity. Numeric QID order then root
sort order is deterministic. Same inputs produce identical bytes. Existing
directories/symlinks refuse unchanged; a failed run removes only its own newly
created directory. Consumers refuse partial directories lacking the marker.

The prepared SQL is a single transaction with fixed table locks and pinned
namespace/string settings. It replaces only Wikidata artist memberships, joins
against the artist mirror, atomically replaces roll-ups/overrides and the
snapshot receipt, and updates root/genre metadata. Removed roots remain reserved
and retired; old genre identities remain for provenance. Non-Wikidata rows are
never produced or deleted. Manual/file-tag **representation** is not permission
to ingest them. `artist_genre_root` chooses only manual rows if any exist,
even when unresolved; otherwise it unions Wikidata/file-tag roots. A refresh
must supply all ancestry needed by existing authorized assignments; absent
ancestry becomes unresolved, never a fallback to lower-priority assignments.
Generated `coverage.json` measures the supplied Wikidata snapshot only, **not**
pre-existing manual/file-tag database rows. A later full database export must
read `artist_genre_root` for those; do not call this offline file an after-all-legs
real-data measurement. Numbered integration must verify identical logical state
on fixture replay and whole-refresh rollback on failure against the actual
schema. Those database behaviors are intended, not proved by this schema-free
preparation. No grants or automatic runtime are added.

`GET /v1/genres` is an authenticated empty read stub, not a live mirror read or
a coverage assertion. It follows W9 session-or-bearer `library:read`, bare-array,
opaque-503 and bounded first-page (50) conventions. No cursor/query transport,
per-genre detail, UI or changes to `/api/library`/the M1 journey are introduced.

## Verification

GitHub Actions `mb-import-etl` runs resolver and materialization tests; the
database replay/rollback harness must be integrated with the actual ordered
migration before DB-backed integration is complete; it is not wired to a
candidate schema here. API tests run in the existing Rust jobs. Full suites/CI gates run only on Actions, not locally;
focused reproduction follows [CONTRIBUTING.md](../../CONTRIBUTING.md#verification-github-actions-first).
No schema test uses staging or production.

Operational refresh intent is documented in [ops](../../docs/ops.md#genre-mirror-refresh-intent-disabled).
REA angle: reuse the landed extractor, coverage calculator, W6b mirror isolation
and W9 auth/error/page conventions; no new framework or enabled cadence.
