# Decision 0002 — MetaBrainz: free data only (2026-11-03)

Date: 2026-11-03. Provenance: owner spending decision recorded in the
approved programme document (version 1.4) and architecture decision
record 0012, amending architecture decision record 0007.

## Context

The product mirror needs public music data. Core data is unrestricted;
supplementary data carries a non-commercial share-alike licence that
is incompatible with a paid product. Hourly replication packets carry
the same licence.

## Decision

Use free data only. No supporter tier, no account or token. The
mirror uses core dumps only, refreshed twice weekly. The hourly live
feed and all non-commercial share-alike material (tag and genre
assignments, ratings, annotations) are out. Every item whose planned
data source was that material moves to a later conditional set
pending a licence-compatible genre evaluation.

## Why

The service is a private, invite-only hobby service; the re-open
trigger is real revenue. The genre list itself is core data, but
genre assignments to entities are not, so genre browsing depends on
licence-compatible coverage. Live API etiquette: at most one request
per second per address with a descriptive user agent; product reads
come from the mirror, not the live API.

See [AGENTS.md](../../AGENTS.md#metabrainz-licences-musicbrainz-listenbrainz-critiquebrainz-cover-art)
for the full licence rules and
[docs/adr/0007-mb-mirror.md](../adr/0007-mb-mirror.md).

## Amended 2026-11-07

The refresh cadence above was superseded by the 2026-11-07 amendment in
[docs/adr/0007-mb-mirror.md](../adr/0007-mb-mirror.md#2026-11-07-amendment-retirement-and-conditional-bounded-replacement):

- Refreshes use weekly CC0 full exports; twice-weekly only on
  demonstrated product need.
- Weekly automation remains disabled until its non-Container runtime and
  per-table streaming/headroom requirements are resolved.
- The former mirror was retired on 2026-11-06.
- The bounded replacement is not an imported or activated read service.

The CC0-only decision itself is unchanged.
