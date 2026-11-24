# Inert Subsonic snapshot database transfer

The production adapter remains unmounted. Its database probes enforce the
following limits on projected results before the corresponding data fetch.

| Result | Admitted rows | Rejected sentinel | Projected text |
| --- | ---: | ---: | --- |
| App releases | 2,048 | 2,049 | Up to 36 octets per row |
| App release/recording pairs | 2,048 | 2,049 | Up to 72 octets per row |
| Mirror artists and releases | 1,024 each | 1,025 each | Up to 5,120 octets per row and 262,144 per result |
| Mirror track occurrences | 1,024 | 1,025 | Up to 6,144 octets per row and 262,144 per result |

The app counts distinct releases or pairs before Rust filters malformed IDs.
SQL replaces IDs over 36 octets with empty strings. The two app pages allow
221,184 projected text octets in total. Empty membership avoids the mirror.
The mirror caps each of five artist fields, five release fields, and six track
fields at 1,024 octets. Its three result caps allow 786,432 text octets in
total. These figures exclude fixed-width columns, null and framing metadata,
query binds, driver buffers, and Rust copies. PostgreSQL measures server
encoding octets. The production transfer calculation assumes UTF-8 server and
client encoding, which still needs confirmation on the candidate connections.

These provisional limits are finite engineering bounds, not measurements of
real catalogue distributions. An oversized mirror field, oversized result,
or sentinel page fails the snapshot. Earlier admitted pages may already have
been fetched when a later probe fails.

Both app membership queries share a read-only repeatable-read transaction. All
mirror preflights and fetches share a separate read-only repeatable-read
transaction. Each has a local statement timeout within the overall deadline.
App and mirror state can come from different commits. Database scans, joins,
deduplication, sorts, concurrent transactions across isolates, and cancellation
or connection teardown remain outside the transfer bound.

The separate staging measurement adapter also admits 1,024 rows per result.
It shares one 262,144-octet source-text allowance across app membership and
the three mirror results, checks both mirror encodings for UTF-8, and reserves
charged projection capacity before streaming admitted rows. See the [staging
construction charge](subsonic-staging-charge.md) for the ledger boundary.

Catalogue work follows the Clean-room, YouTube and MetaBrainz guardrails in
AGENTS.md; no catalogue source or licence policy is changed.
