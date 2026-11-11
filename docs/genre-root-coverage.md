# Playable-artist root coverage (offline v1)

`scripts/genre_root_coverage.py` calculates **artist-count coverage**, not taxonomy
resolution, popularity-proxy coverage or play-weighted coverage. It uses only local
UTF-8 JSON, Python's standard library and no database, network or runtime service.
The integration owner adapts a future producer/export to this contract; this tool
does not change any producer, schema, worker, ingest policy, UI or fallback leg.

## Minimal input contract

One JSON object, maximum 32 MiB, with exactly these fields:

| Field | Type and meaning |
| --- | --- |
| `schema_version` | Integer `1` (not boolean or float). |
| `dataset_kind` | `synthetic` or `actual-export`; explicit caller-supplied provenance label. |
| `playable_artist_mbids` | Array of artist MBIDs for the playable set being measured. This is the entire denominator, not the catalogue or a popularity sample. |
| `resolved_root_ids` | Array of authorized root IDs from the resolved own-taxonomy snapshot. Empty is allowed and yields zero covered artists. |
| `memberships` | Array of objects with exactly `artist_mbid` and `root_id`. Each represents an already-resolved artist-to-root membership; `root_id: null` means unresolved. |

MBIDs must have the hyphenated 8-4-4-4-12 hexadecimal UUID shape, excluding nil;
case normalizes to lowercase. Shape validation does not verify that an MBID exists
or represents an artist. Root IDs are case-sensitive lowercase ASCII slugs matching
`[a-z][a-z0-9-]{0,63}`. Use stable producer IDs, not display names. Duplicate object
fields, extra/missing fields, malformed rows, invalid UTF-8/JSON, non-finite JSON
numbers and invalid identifiers are errors, even on rows outside the playable set.
There are no implicit defaults or best-effort skips for malformed inputs.

## Counting and output

- Denominator: number of **unique** playable artist MBIDs.
- Numerator: number of those artists with **at least one** membership whose root
  is in `resolved_root_ids`. Duplicate rows, duplicate artists and multiple valid
  roots never increase an artist's contribution beyond one.
- Uncovered: denominator minus numerator. Missing, null and unknown-root-only
  memberships leave the playable artist uncovered. Non-playable membership rows
  cannot affect the numerator or denominator.
- Unknown non-null root IDs affecting playable artists are listed once, sorted,
  in `unknown_root_ids_for_playable_artists`, including for otherwise covered artists.
- `percentage_exact` is an exact rational **percentage** string: `80` means 80%,
  `200/3` means exactly 66⅔%. `percentage_display` is rounded to six decimal places
  (half-even); it is presentation only. Numerator and denominator remain exact counts.
- The fixed **>=80%** test uses integer arithmetic (`5 * numerator >= 4 * denominator`),
  never the rounded percentage. An empty denominator has null percentages,
  `status: "no_playable_artists"` and `threshold_met: false`; it cannot manufacture a pass.
- Results use sorted JSON keys, sorted diagnostics, no timestamps or source paths.
  Input ordering and duplicate memberships do not change the output bytes.

CLI exit codes: `0` means this input's numeric threshold was met; `1` means below
threshold or empty; `2` means invalid/unreadable/oversized input (stderr only, no
partial report). The command prints the report to stdout without modifying inputs.

## Synthetic example and verification

```sh
python3 scripts/genre_root_coverage.py scripts/fixtures/genre_root_coverage/synthetic.json
```

This is **synthetic only**: the artificial UUID-shaped identifiers and `root-a` /
`root-b` placeholders are not a real artist set or taxonomy. Expected report:
[`synthetic.expected.json`](../scripts/fixtures/genre_root_coverage/synthetic.expected.json).
It demonstrates 4/5 = 80%, one uncovered artist, duplicate/case normalization,
multi-root, null/unknown roots and an ignored non-playable artist. This numeric
fixture result **does not pass the actual-ingested-set gate**.

Actions runs `scripts/test_genre_root_coverage.py` in the `public-text` job, covered
by `ci-ok`: fixture equality, duplicates, reordered inputs, multi-root, unknown and
non-playable memberships, empty denominator, malformed input, CLI exits, exact
fractional percentages and below/at/above threshold boundaries. Full verification
runs on GitHub Actions under [CONTRIBUTING.md](../CONTRIBUTING.md#verification-github-actions-first),
not as a local suite or CI mirror.

## Later real-data measurement and guardrails

For the later consuming measurement, export the **actual playable N-ingest artist
MBIDs** and resolved own-taxonomy memberships for the same snapshot/scope. Retain
export identity, completeness and snapshot provenance with the report. These are
prerequisites for measuring the real set before the browse/library/catalogue gates,
not prerequisites for implementing or synthetically verifying this calculator.
`actual-export` is only a caller assertion: the calculator cannot attest provenance,
licensing, completeness or freshness, so `real_data_gate` is always `not_evaluated`.
Neither the old popularity proxy nor synthetic fixtures establish real-set coverage.
Below the real-data gate, retain curated-shelf degradation; this tool activates nothing.
Play-weighted coverage is a separate future metric requiring actual play data, which
this contract deliberately does not accept or infer.

Follow AGENTS.md **“Clean-room, YouTube and MetaBrainz guardrails”**: CC0 inputs only;
no MusicBrainz genre/tag assignments, NC supplementary data, MLHD+, competitor
taxonomy/copy/assets, live query services or production reads. File-tag/manual ingest
authorization remains outside this work. REA angle: reuse the W6b offline dump-tooling
separation (local inputs, explicit contracts) and existing library-route conventions
without touching their ownership or introducing a new framework; no W9-specific
implementation was present in this checkout.
