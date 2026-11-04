# Original-source contract v1

## Registry and authority

`track_sources.nzb_ref_payload_format = 1` means **OriginalSourceV1**,
strict UTF-8 JSON for exactly one original FLAC file. It is not the raw NZB
upload format of `ingest_originals`, nor the legacy playback manifest.
`nzb_ref_envelope_version` remains **2**. Unknown versions/formats fail closed;
there is no format sniffing or compatibility fallback.

This is an independently testable producer/service/reader prerequisite.
There is **no production catalogue, Worker-service, publish-transaction or
provider adapter wired by this change**. The file-backed synthetic conformance
test uses the real pure producer, source INSERT and lookup SQL (translated to
SQLite's dialect), API seal/open, and bounded media reader. It is not a
Hyperdrive/Workers qualification test or successful curated staging probe.

Ingest derives a file selection from trusted job/match data and the
DB-selected winning track. The API alone seals and opens using the existing
keyring authority. Stream/ingest receive neither KEKs nor DEKs. The service
requires the trusted request authority's authorization check before lookup;
it exposes no raw-source HTTP endpoint. Future adapters must implement that
authority rather than treating a client-supplied track id as authorization.

## Payload

All object fields are fixed; unknown/duplicate fields, non-integer numbers,
invalid UTF-8, excessive depth, trailing data and truncation are rejected.
No filename, subject, newsgroup, provider, credential, external URL, media key,
or album-wide article list is stored.

| Field | Meaning |
| --- | --- |
| `track_id` | Canonical lowercase global track UUID selected after natural-key resolution |
| `source_job_id` | Validated persisted publishing job id, compared on read with the selected row |
| `file_ordinal` | Original zero-based document ordinal within that job |
| `file_size` | Positive decoded file byte count, not encoded NZB byte sum |
| `article_count` | Exact length of `articles` |
| `articles` | Ordered entries with `number`, `reference`, `encoded_bytes` |
| `range_hint` | Optional/null object with `part_size`, `final_begin`, `final_end` (half-open decoded offsets) |

Article numbers are exactly `1..N`, and array indices are `0..N-1`.
References use the existing core normalization rule and must already be in
normalized form; duplicates are rejected. Declared encoded sizes are positive
and never become plaintext offsets. The source registry's reference bound does
not enlarge any transport's stricter wire identity limit.

`original_source_for_track` repeats the established pairing: content files in
document-ordinal order against recordings in album order, without shifting
pairs when a recording is skipped. It rejects duplicate file ordinals and
compares the selected release/medium/track/recording identity with the derived
row. The caller must obtain `SourceTrack` from the persisted winning row,
never construct a proposed id before an upsert that can conflict. Only the
selected content/FLAC file's articles enter the record.

The existing publication owner must persist the complete sealed value in the
existing album-atomic transaction behind the persisted-job fence. A losing
`ON CONFLICT (track_id) DO NOTHING` keeps the previous **whole** source,
including its file binding and hint. No losing hint is attached separately.
This prerequisite does not replace the existing correction/publication path.

## Envelope

The API reuses the provider v2 implementation factored into `field_envelope`:
AES-256-GCM, a fresh row DEK, independent random 12-byte nonces, and a 48-byte
wrapped DEK. Entropy failure fails closed; transient DEK/plaintext buffers use
the existing zeroizing discipline. The legacy user-bound v1 vault helper is
not used for shared sources.

AAD is the existing ordered length-framed encoding (`u32` big-endian byte
length followed by UTF-8 for each string):

1. `nntune-envelope-v2`
2. purpose: `payload` or `dek`
3. table: `track_sources`
4. field: `nzb_ref`
5. owner scope: `shared:track-source`
6. row: selected canonical global track UUID
7. envelope version: `u16` big-endian `2`
8. payload format: `u16` big-endian `1`
9. **Wrap only:** exact KEK version, `u32` big-endian

Expected AAD comes from the selected row/fixed constants, never stored claims
alongside ciphertext. `KekSource::kek` resolves only that exact version, with
no key trial/fallback. Source-job and file selection are authenticated inside
the payload; the selected source job must match. Payload AAD omits KEK version
so the existing wrap-only rotation/CAS model is retained. No rotation job,
key store, credential creation or new cipher is introduced.

## Bounds

| Resource | Ceiling |
| --- | --- |
| Plaintext source | Positive, at most 8 MiB |
| Ciphertext | At most 8 MiB + 16-byte GCM tag |
| Lookup | Exactly one global track; bounded two-row metadata projection detects an invalid duplicate |
| Files represented | One; original ordinal below 1024 |
| Articles | 1–4096, no reserve from stored count/sequence hints |
| Reference | 1–998 decoded bytes, no brackets/whitespace/control bytes |
| Encoded declaration | Each 1–16,777,216 bytes inclusive (`MAX_ARTICLE_BYTES`); checked sum at most 64 GiB |
| Decoded file | 1–64 GiB; never a file-sized allocation |
| Actual encoded/decoded article | At most 16 MiB each |
| Traversal | At most N opens and at most the bounded file's total decoded bytes |

`source_metadata_sql` projects lengths, not BYTEA, and bounds the job text.
After shape/version/key checks, `source_envelope_sql` checks every byte-bearing
column's length in SQL and pins the selected job, KEK and ciphertext length.
Nonce conversion and decryption happen only afterwards. An adapter must not
replace these steps with an unrestricted row/base64/JSON fetch followed by a
late size check. A concurrent change loses the selected snapshot or fails
closed at authentication/binding checks. Metadata uses Rust `i32` for the
PostgreSQL `INTEGER` KEK version and `octet_length(BYTEA)` projections, including
the KEK/length snapshot parameters of the byte-bearing query. Sealing checks
the keyring's `u32` version fits the persisted `INTEGER` domain before use;
reading checks the stored nonnegative version before exact keyring selection.

JSON lexical preflight bounds each string token (including escaped forms),
its decoded byte count, numeric token length and nesting **before** structured
parsing allocates fields. A private wire type and bounded sequence visitor
prevent bypass through public deserialization or unchecked capacity hints.
Public source values can be created only by the validated producer/parser.
Both enforce the declared article cap before a source can be sealed or passed
to transport, including the final article required by every range read. The
general NZB `MAX_SEGMENT_BYTES` intake limit is unchanged.

The reader collects one encoded article in capped chunks and decodes into a
bounded destination, reusing core yEnc. It retains the final decoded article
while walking the prefix, rather than fetching it twice or buffering all
replies. Working memory is bounded by record plus a constant number of
article buffers, not file size. Transport adapters own cancellable blocked I/O
and operation deadlines; the reader checks the authority's control before I/O,
through collection and before output. A sink must stream, not collect a file.

## Original integrity and ranges

The envelope authenticates source metadata and row binding. **It does not
authenticate original NNTP media cryptographically.** Original bytes are
yEnc-decoded, not decrypted with a new media cipher. Core yEnc framing,
length and applicable CRC checks remain authoritative: multipart uses part
CRC, not a whole-file CRC over a part; absent optional checksums are not
promoted to authentication. STAT availability is not media integrity proof.

Range inputs are validated half-open intervals; the HTTP authority retains
its existing range parsing/response semantics. Inverted, empty, overflowing or
out-of-file ranges do not reach unchecked slicing. The actual final article
must declare the expected file size and end at it before any output. Single
part files require one non-multipart article whose decoded length is the file
size; no multipart header is invented.

A uniform hint is corroborated only if P comes from the first decoded part,
`ceil(S/P) = N`, and actual first/final intervals agree with
`[0,min(P,S))` and `[(N-1)P,S)`. Checked candidates are
`[iP,min((i+1)P,S))`; every intermediate part must still corroborate actual
geometry. A hint mismatch discards the hint. Contradictory actual file sizes,
intervals, decoded counts or checksums fail, rather than becoming a hint miss.

The implemented reader deliberately uses the **verified contiguous prefix
walk even with a hint**. This is the safe fallback, not an accelerated direct
seek implementation. `UniformRanges` supplies corroborated candidates for
later integrations without making a hint a new publication authority. An
error after streaming output requires the response authority to abort, never
claim a complete file or retry bytes into the same response.

## Async reader and off-route response adapter

`original_source::reader::RangeReader::new/next_reference/push` is the shared
incremental reader for synchronous and asynchronous consumers. `read_range`
uses that same state machine; `AsyncRangeReader` only adapts asynchronous article
fetches, cancellation and bounded output scheduling. There is one bounded decode,
final-first proof, aggregate budget and contiguous prefix implementation.

A successful `push` can emit both the current and retained final article slices.
The async wrapper queues at most two article-sized slices and releases one per
pull, with no new dispatch while buffered output remains. Cancellation is checked
again before buffered output, including the retained final slice. A failed push
poisons the incremental reader; fetch failure or a dropped pending await also
permanently stops the async wrapper. Neither permits retry after uncertainty.
`AsyncArticleSource` supplies one bounded encoded article by trusted
source index; it owns wire collection limits, its operation timer and account
physical-close/capacity obligations. `AsyncReadControl` combines boundary checks
with a wakeable cancellation/deadline signal, so a pending fetch can be dropped.
A failed or dropped pending pull stops the reader permanently: no retry after
an uncertain operation. Dropping an unpolled pull performs no I/O.

`workers/stream/src/original_play.rs` is an **off-route** response driver, not a
public miss-path installation. `authorize_original` verifies the v2 capability
before a caller resolves the source. After resolution, `prepare` rechecks the
request authority and track binding and reuses the existing HTTP range parser.
Its request control must wake at grant expiry as well as cancellation/deadlines;
the future production account supplier still owns its at-most-20-second timer.
The API remains the sole source envelope/key/database owner. No new service
wire shape, source resolver, account registry or provider config is introduced.

A returned chunk is not delivery evidence. The response adapter acknowledges
only the ordered prefix accepted by its writer using `written(n)`, then either
acknowledges the rest before pulling again or drops the response. `finish`,
`abort`, errors and drop settle at most one interval of acknowledged bytes;
zero bytes emit none. Verification/prefix bytes and unacknowledged pulls never
count. There is no cache-fill drain, speculative prefetch or account release in
this driver. An article/source failure does not establish unknown-track 404.

`OriginalPlay::pump` owns an ordered, wakeable `OriginalWriter` until close.
It awaits each accepted prefix before pulling another article, observes reader
disconnect during both pending article and writer operations, and races request
cancellation/expiry against each write and close. Dropping even an unpolled pump
aborts its writer without source I/O; an error or uncertain write is never retried.
A failed close still reports only the bytes already acknowledged.

The off-route wasm `original_play::worker::response_channel` pairs the prepared
response with the pinned SDK's fixed-length Web Stream and a concrete writer.
It copies at most one bounded slice into a JS chunk, waits for that write promise,
and only then acknowledges it. Its `closed` promise wakes a pending article on
reader disconnect; drop requests an idempotent writer abort. Promise resolution
means sink acceptance, not proof of remote TCP receipt. The caller must return
the response concurrently with pumping, provide a wakeable request/grant control
and persist delivered reports; no runtime caller or account supplier is installed.

`WorkerOriginalControl::new` binds the verified grant expiry and incoming
request's abort signal before source resolution. The request owns the abort
listener and an absolute-expiry timer; drop removes the listener and cancels a
pending SDK delay. Timer delays are computed when polled and clamped to the
SDK's signed timeout range, so delayed polling cannot extend the grant. A stable
wake relay forwards both abort and timer events to the latest task waker, even
when the SDK delay keeps its first registration. The portable
`OriginalRequestControl` tests the relay and terminal expiry with injected
clocks/timers; neither a cancellation handle nor the control releases account
capacity. The supplier's operation timer is still separate. This wrapper is
not called by any public fetch handler, and actual Worker signal/timer behavior
remains unqualified.

Generated async fixtures exercise lazy full/range output, hint fallback,
endpoint/CRC/budget failures, wakeable pending cancellation, dropped pulls,
grant expiry after awaits and partial-writer accounting. Writer fakes cover
backpressure, partial/invalid acknowledgements, disconnect races, dropped pumps,
article/write/close failures and exactly-once settlement. Control fakes store and
replace the task waker: cancellation and grant expiry explicitly wake pending
articles, writes and closes, win ready write/close races, and preserve only the
acknowledged range prefix. Pending close also covers writer wake-to-success and
drop-to-abort. These are synthetic events, not real Worker timer or AbortSignal
qualification. Existing cache-hit and public-route fixtures remain regressions,
not evidence of a newly wired
original-source route. Actual source/catalogue/account composition, Worker
writer/abort behavior, R2 misses and cold-cache end-to-end operation remain
unimplemented/unqualified here. Compile-time dialing/ingress gates are unchanged.

## Acceptance boundary and verification

Generated/redacted fixtures cover persisted per-track/file readback, interleaved
non-content files, ordering, invalid recording/release selection, duplicates,
first-writer conflicts and replay; domain/AAD/job/track substitution and unknown
KEKs; malformed/oversized/truncated JSON and envelopes; count/string/depth/size
bounds; actual yEnc integrity and geometry, single/final parts, invalid hints,
prefix fallback and invalid ranges/cancellation.

Useful targeted commands (all compilation stays in the bounded Cargo pool):

```sh
python3 scripts/cargo_cache.py run -- test -p nntune-core --test original_source
python3 scripts/cargo_cache.py run -- test --manifest-path workers/api/Cargo.toml --lib original_source::tests
python3 scripts/cargo_cache.py run -- test --manifest-path workers/ingest/Cargo.toml --lib track_writers::tests
```

Full STAT-all, first/last BODY/FLAC probes, live persisted publish wiring,
Worker qualification, curated staging acceptance and independent full-slice
security review are still required by their existing owners. Posted-file key
wrapping, ranged decrypt and `segment_ranges` are separate contracts. Dialing
and ingress remain compile-time disabled. No real-provider operation,
credential access, content publication or deployment is authorized here.

AGENTS.md **Clean-room, YouTube and MetaBrainz guardrails** apply. This change
introduces no competitor material, YouTube playback, artwork caching, media
content fixtures or supplementary non-CC0 catalogue data.

REA angle: reuse the shipped writer pairing/fence, canonical v2 envelope and
exact-version keyring, NZB normalization and core yEnc invariants; no second
manifest store, cryptographic format or provider configuration source.
