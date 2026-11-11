# Offline account-export format, version 1

This is a **pure offline foundation**, not a live exporter or completion of the
account-export feature. `scripts/account_export.py` accepts typed caller-supplied
snapshots and returns in-memory ZIP bytes. It has no command-line entry point,
filesystem writes, SQL, provider calls, auth, operation lifecycle or app wiring.
It uses Python's existing standard-library `zipfile`, `csv`, `json` and `datetime`
tools rather than introducing an archive dependency or a hand-written ZIP codec.
Engineering review accepts the DTO/format contract before source acceptance.

## Trust and collection boundary

The caller must collect an account-consistent snapshot, using account-scoped
queries, before constructing these DTOs. `Owned(owner_user_id, value)` is an
explicit projection filter, **not authorization**, and cannot prove future SQL
is safe. Every nested membership/entry is filtered as well as every top-level
row. Shared rows (`owner_user_id=None`) and foreign rows are omitted before
projecting their values. No caller-supplied debug representation is serialized.

`Snapshot` requires an `Account`, supplied UTC `generated_at`, and the complete
`SECTIONS` mapping. There are no default empty sections. Missing/unknown sections,
unknown coverage, wrong DTO types, invalid numbers/Unicode/timestamps, duplicate
playlist public IDs/positions and unsafe legacy identity shapes fail the whole
operation with a static `ExportError`; no archive or partial result is returned.
An absent account represents erased/unavailable input and fails. An account with
`deleting_at` set is accepted; this does **not** enforce the deletion grace window
or synchronize with erasure. No custom exceptions include source values.

Each section is `{ "status": "collected" | "not_collected", "rows": [...] }`.
`collected` with no rows means an intentionally collected empty source;
`not_collected` requires an empty list. The serializer cannot infer source
completeness from an empty list. Adapters must never substitute empty collected
sources for unavailable/failed reads. Unsupported future sources can only be
`not_collected`, until their DTOs and tests are accepted in a later change.

## Stable JSON keys and allowed fields

`account.json` contains `schema_version: 1`, `generated_at`, `account` and exactly
the section keys below. All times are ISO 8601 UTC with six fractional digits and
`Z`. DTOs require `datetime` values using `timezone.utc`; `utc_from_epoch` converts
legacy integer **seconds**, with checked platform date range, at the DTO boundary.
No internal sequence keys are accepted as identities. Nullable dates remain null;
adapters must not manufacture dates absent from stored data.

| Key | Allowed row fields |
| --- | --- |
| `account` (single object) | id, email, created_at, deleting_at |
| `sign_in_methods` | cred_id, registered_at, sign_count |
| `sessions` | id (first eight characters only), created_at, expires_at, revoked |
| `app_keys` | label, client_name, key_prefix, username, scopes, auth_mode, created_at, expires_at, last_used_at, revoked_at, created_from_ip |
| `libraries` | name, created_at, tracks (nested source section) |
| `legacy_library` | added_at, track (only account-owned legacy memberships) |
| `playlists` | public_id, name, is_public, created_at, entries (nested source section), legacy_items |
| `genres` | public_id, name, is_public, created_at |
| `listening_history` | track, recorded_at, stream_seconds |
| `usage_totals` | period_start, period_end, stream_seconds, ingest_jobs |
| `billing_accounts` | plan, status |
| `subscriptions` | plan, status, period_start, period_end, cancel_at_period_end |
| `invoices` | stripe_invoice_id, amount (stored minor units), currency, paid_at |
| `billing_operations` | kind, state, created_at, updated_at |
| `deletion_state` | scope, state, started_at, erase_after |
| `security_activity` | action, ok, at (source_ip is deliberately absent pending Security acceptance) |
| `legacy_provider_vault` | exists, updated_at (no decrypted contents) |
| `subsonic_credentials` | not_collected only; existing subsonic client metadata belongs in app_keys |
| `listenbrainz_links` | not_collected only |
| `follows` | not_collected only |

A nested library membership has `added_at` and `track`. A nested playlist entry
has `position` and `track`. Entries sort by position, retaining gaps. Every track
has `title`, `artist`, `release`, `recording_mbid`, `release_mbid`, `artist_mbids`,
`duration_seconds`, `available`, and `catalogue_missing`. MBIDs are validated UUID
strings and carry only CC0 identity, not supplementary tags or ratings. Unknown
identity text may be empty when that is all the caller retains. Unavailable and
catalogue-missing entries are retained; a missing catalogue cannot be available.
`unavailable` is also explicit in playlist CSV as the inverse of `available`.

Legacy playlist storage is an opaque string array (migration 0003). `legacy_items`
preserves the exact order and bytes of accepted identity strings, including
unresolved references. It never attempts to resolve them against a live catalogue.
Version 1 accepts nonempty opaque strings, including punctuation and Unicode, not
URL/protocol-relative locations or arbitrary objects. Unsupported legacy shapes
fail the complete archive, never disappear into
an empty list. This boundary is not permission to pass arbitrary JSONB, secrets or
media locators through a new field; an expanded representation needs review.

These are semantic export DTOs, **not a new database schema**. In particular,
usage period boundaries are caller-converted UTC dates and the current daily
ledger's counters are stream seconds and accepted ingest jobs. Billing fields
may require an accepted account-scoped join; this helper performs none.

## Archive and text formats

The ZIP contains `account.json`, `library.csv`, `listening_history.csv`,
`README.txt`, and one CSV and M3U8 per projected playlist. README states supplied
generation time, explains every file, maps playlist files to public identities,
lists exclusions, and reports every source's status/count. Nested source coverage
is authoritative in JSON. Even an empty/not-collected CSV has its header; it is
never proof that a source was collected.

ZIP members sort lexically, use STORE (no compression/version-dependent codec),
a fixed 1980 timestamp, Unix regular-file permissions 0600, and no extra metadata
or archive comment. The same snapshot and generation time yield identical bytes.
Unordered section rows sort by canonical allowed JSON; playlist entries sort by
position. `legacy_items` and scope/artist identifier arrays retain supplied order.
No wall-clock, RNG, host path, platform filename or filesystem access is involved.

Playlist names are `playlists/<six-digit-ordinal>-<ASCII-slug>.csv` and `.m3u8`.
Slugs are bounded and never contain separators or dots; a blank slug is `playlist`.
The ordinal prevents silent collisions across duplicate, case-only, empty,
Unicode and path-like names, including on case-insensitive extractors. Original
names and public IDs remain in JSON; CSV rows also include public playlist ID.

CSV is UTF-8 with CRLF record endings and standard quoting of commas, quotes and
embedded newlines. Cells whose first non-whitespace character is `=`, `+`, `-` or
`@` get an apostrophe prefix against spreadsheet formula interpretation. Control
characters other than quoted CR/LF are visibly escaped. JSON preserves the exact
source text, so these presentation transformations do not lose identity.

M3U8 is **metadata only**: `#EXTM3U`, a comment and `#EXTINF` records, never media
locations or playable entries. Embedded line/control characters in labels are
visibly escaped to prevent directive injection. Unknown duration is `-1`.
This deliberately is not a playlist capable of starting playback.

Projection rejects more than 100,000 aggregate rows/tuple elements, individual
strings above 64 KiB UTF-8, more than 8 MiB aggregate projected text, or file
contents above 64 MiB. A single shared output budget charges each UTF-8 chunk
before retaining it, including repeated CSV identifiers, so a small input cannot
amplify into an unbounded CSV before the size check. Limits fail the entire export;
there is no truncation. ZIP64 is disabled. This bounded in-memory serializer is not a large-account
streaming strategy; a future integration must handle a limit as failure, not offer
a partial ZIP, and may require a separately reviewed streaming implementation.

## Exclusions and remaining integration gates

The [AGENTS.md clean-room, YouTube and MetaBrainz guardrails](../AGENTS.md#clean-room-youtube-and-metabrainz-guardrails)
apply: no shared catalogue beyond allowed retained identity, media/NZB/cover art
or locations, NC material, other users' records, secret/verifier fields, provider
vault contents, request/webhook bodies or operational/transient tables. Source
adapters remain responsible for selecting lawful identity data; this is not a
content classifier for arbitrary strings in otherwise permitted text fields.

Before enabling a live export, separate accepted work must supply collection,
auth/privacy/fresh step-up, atomic generation/erasure behavior, async single-flight
delivery, authenticated expiry, retention, rate limits and accessible UI/errors.
No such consuming cuts are authorized or implemented here. Format tests run in
GitHub Actions via `python3 scripts/test_account_export.py`; local full-suite runs
are not CI evidence.
