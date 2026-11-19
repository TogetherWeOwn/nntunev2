# Inert Subsonic snapshot database transfer

The adapter remains unmounted. These limits bound database result transfer,
not response lifetime, concurrent requests or aggregate isolate memory. They do
not qualify activation.

The mirror result sentinel is 1,024 rows: a full result is rejected, leaving
at most 1,023 rows per result. Each app membership query returns at most 2,048
distinct releases or release/recording pairs, and rejects a full result before
filtering or connecting to the mirror. The factor of two permits some unmatched
or malformed entries without promising that every otherwise-fitting library is
admitted. IDs over 36 octets are replaced with invalid empty values in SQL,
so existing malformed-ID filtering still applies without transferring them.
The maximum admitted app ID text is 2,047 × 36 bytes for releases plus twice
that for pairs, excluding protocol overhead.

These provisional limits are finite engineering bounds, not measurements of
real catalogue distributions or a Worker memory budget. Large libraries or
malformed membership floods may now fail rather than return partial results;
the previous mirror sentinel was 100,000 and membership was unbounded. Real
library compatibility must be evaluated before activation. Empty membership
still avoids the mirror.

The mirror checks selected text on the database before fetching rows: each
field is limited to 1,024 octets and each result to 262,144 text octets. Across
three results this limits text to 768 KiB, plus fixed-width fields and protocol
overhead. Oversized values or aggregate metadata fail rather than truncate;
the compatibility impact on real libraries is unknown. These provisional limits
are conservative admission choices, not a calibrated isolate-memory budget.

Both app membership queries share a read-only repeatable-read transaction. All
mirror preflights and fetches share a separate read-only repeatable-read
transaction. Each has a local statement timeout within the overall deadline;
the two transactions do not provide a global atomic snapshot. Limits bound
client transfer, not database execution work or storage.

Catalogue work follows the Clean-room, YouTube and MetaBrainz guardrails in
AGENTS.md; no catalogue source or licence policy is changed.
