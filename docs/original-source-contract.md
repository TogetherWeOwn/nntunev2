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

## Async playback composition

`reader::RangeReader` exposes that same bounded walk incrementally: await
only `next_reference()`, then `push` its framed body. The synchronous
`read_range` also uses this state machine. It retains the final decoded part
for proof, never fetches it twice, checks the remaining aggregate decoded
budget before another open, and poisons a walk after any failed push. A
completed walk requests no further articles. Transport cancellation and a
whole-request deadline remain the caller's responsibility.

The internal `account_playback::read_original_range` composition verifies a
v2 play grant before the trusted source lookup, rechecks its track binding
and expiry before provider work and output, and dispatches only fixed Stream
BODY requests. A caller-owned deadline of at most 20 seconds covers the
lookup and the entire range walk; each article has at most one dispatch,
without failure/uncertainty retries. The sink streams verified slices and
must abort a partial response on error. Its internal receipt counts slices
accepted by the sink, **not bytes delivered to a client**, and records actual
serving-account identities only as telemetry. It confers no cache authority.

This composition installs no production catalogue/service binding, public
route, output backpressure adapter, writer execution or metering call site.
It does not decrypt posted files or claim cryptographic original-media
authentication. Runtime qualification gates remain disabled and mandatory.

## Shared request authority preparation (off-route)

`account_playback::control::RequestControl` is a local authority primitive for
future catalogue/account-supplier and response composition. It verifies a
canonical track-bound v2 play grant before constructing its timer. One timer
covers the earlier of the request deadline and grant expiry, with exact
remaining milliseconds. The injected timer and abort futures must wake the
current task; native tests do not qualify platform timer or request-abort APIs.

Clones share terminal state and a monotonic clock watermark. Clock rollback,
expiry, explicit stop, a failed enclosed call or dropping a polled pending call
permanently revoke every clone. Abort/deadline signals and explicit stop wake
live handles; dropping a sibling cannot strand the remaining signal waiter.
The last handle owns signal cleanup. `run` checks cancellation before and after
polling work, so control wins a simultaneous success. An unpolled call does no
I/O and does not revoke authority. `check` is a synchronous boundary check;
`poll_cancel` supplies the wakeable seam. `now_unix` returns the last checked
clock observation and must only be used after checking authority.

This authority implements the shared `ReadControl`, `AsyncReadControl` and
`OriginalControl` traits. Async cancellation and observed time forward directly
to its inherent methods: the supplier and response use clones of the SAME
checked authority, not another clock watermark, request timer or cancellation
controller. The supplier still binds source index/reference to its validated
catalogue and enforces its own at-most-20-second operation timer. No runtime
caller is installed by this forwarding. Local revocation never proves physical
account closure, releases capacity or records delivery.

## Catalogue/account article supplier (off-route)

`account_playback::supplier::ArticleSupplier::prepare` takes a clone of the
already verified request authority and performs one trusted source lookup under
that authority. Missing, failed or wrong-track lookups poison the shared control.
It retains the validated source; each inherent `open(index, reference)` binds
both the zero-based ordinal and the exact reference to that source. The final
article may be requested first, but no ordinal can be attempted twice and total
attempts cannot exceed the source's article count. Dropping an unpolled call
performs no I/O; dropping a polled pending call revokes the shared authority.

Every article open creates one injected operation timer for the earlier of
20 seconds or the request/grant deadline. It spans registry, all candidate hints
and the single fixed-Stream BODY dispatch without resetting at stage boundaries.
Request cancellation and the operation timer are checked before and after each
individual I/O poll, including synchronously-ready results, so expiry or clock
rollback cannot advance selection to another stage. A hint failure can still be
ignored by selection; no second account is tried after an operation dispatch,
including a lost, invalid, empty, oversized or failed reply. Every terminal
supplier error revokes the response control too.

The account transport owns bounded reads and its server retains independent
physical-close/capacity authority when client work is cancelled. A successful
open yields a bounded encoded article, not verified media. Successful serving
identities (including proof articles) are internal telemetry only: no public
metadata, cache authority, entitlement or delivered-byte meter is created.

`AsyncArticleSource::open` forwards directly to this same inherent operation.
It can feed the shared `AsyncRangeReader` and off-route `OriginalPlay` response
without a second reader, writer or meter. The request control passed to that
response must be a clone of the supplier's already checked authority. There is
no Worker supplier/timer installation, public caller or newly enabled route in
this increment. Native generated fixtures do not qualify actual Worker abort,
timers or physical close; dialing/ingress gates, full-slice security and curated
staging remain mandatory.

## Internal catalogue transport (off-route)

`SourceCatalogue` verifies the v2 play grant and signs a canonical-track lookup
with the existing play-grant key in a distinct HMAC domain. The attestation's
absolute deadline is at most five seconds and never outlives the grant. The
API service composition authenticates before lazy repository connection and
rechecks authority across every suspension. This is not a public raw-source
endpoint or a new entitlement token.

The qualification-only `WorkerSourceTransport` makes one POST to
`/internal/v1/original-source` through the `API` service binding; it has no
public-fetch, redirect-following, retry or cache path. Its successful internal
reply contract is status 200, `Content-Type: application/octet-stream`,
`Cache-Control: no-store`, and `X-NNTune-Source-Job` containing the persisted
job identity (1–64 validated ASCII bytes). The body is the existing source
encoding, not a second payload format. Content encoding must be absent or
identity. Optional content length must be positive, at most 8 MiB and equal
the final read length. Unknown/missing job identity, bad headers, empty or
oversized bodies and failed reads are opaque catalogue failures.

The whole-call timer covers fetch and body consumption without resetting per
chunk. Monotonic authority checks run before/after I/O and before returning;
zero-progress chunks fail closed. The raw Worker reader checks each typed
array's length before copying into the bounded Rust destination. The pending
fetch and returned reader retain an abort controller until drop, including
error, timeout and caller cancellation. Native fakes exercise that ownership
contract, not actual Worker abort propagation or physical connection closure.

The off-route `WorkerRepositoryFactory` accesses only the existing Hyperdrive
binding, after authentication. It requires the existing query-cache-disabled
attestation and PostgreSQL STARTTLS with no plaintext fallback. A retained raw
socket owns forced-close initiation through opening and handshake, updating
that authority to the replacement TLS socket before PostgreSQL authentication.
The returned `DrivenRepository` polls its driver alongside bounded queries;
no detached connection task is spawned. Driver completion, query failure or
query cancellation poisons the repository and initiates closure once. Dropping
the repository closes before destroying its client/driver. This is not proof of
actual Worker physical closure. Native PostgreSQL tests prove inline driver
progress, not the Hyperdrive/Worker transport boundary.

`service::read_timed` authenticates first, then uses one platform timer for the
signed time remaining through connection, both queries and encoding. Expiry
wins ties, authority is rechecked across suspension and no per-query timer
reset or retry is introduced. The injected-deadline seam remains available for
deterministic tests.

The portable off-route `service::http::handle` accepts only the fixed POST,
JSON content type, a bounded authorization header, no content encoding and no
browser context. It collects at most 512 request bytes, rejects zero-progress
chunks and requires any declared length to equal the actual body. Its raw body
adapter must check chunk length before copying into Rust and cancel reads on
drop. Wakeable caller cancellation and a whole-request deadline enclose body
collection and database work; the signed timer additionally bounds the
remaining authenticated lookup. Controls win ties, including when they become
ready during a work poll. A backward clock poisons the entire request.

Success returns the exact existing encoded source and selected-job identity;
every failure returns the same empty 503 with no job header. Both responses
are explicitly no-store. The off-route `service::worker::handle` candidate
preflights raw JavaScript URL/header lengths before Rust conversion and raw
chunk lengths before copying. It binds an abort-event listener to a retained
waker, removes that listener on drop, cancels the body reader on drop and
supplies one five-second request timer in addition to the signed remaining
time. It materializes only the exact source/job response, with explicit content
length and no cache, CORS, compression or fallback middleware. Neither native
fakes nor compilation establish actual Worker abort propagation or database
physical closure.

No runtime caller, binding configuration or API ingress is installed here; the
existing account/provider qualification gates are not changed. Playback HTTP
response/backpressure, ACK/drop accounting and delivered-byte metering remain
in their separate adapter, not in this source-record transport.

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
When skipping a prefix, the async wrapper self-wakes and yields after each
non-emitting prefix article, even if the supplier is immediately ready. The
initial final-part proof and first prefix article may share a poll; each further
skipped article returns control to the executor before the next fetch. This is
cooperative polling, not proof of host-event-loop or timer fairness. Dropping a
pull at this yield also stops it permanently.
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
and only then acknowledges it. Its `closed` promise wakes pending article, write
and close operations on reader disconnect; a rejected promise cancels in every
phase. A fulfilled
`closed` promise after our close starts is not cancellation: the separate close
promise still determines completion. Its settlement is cached, never re-polled.
Drop requests an idempotent writer abort. Promise resolution
means sink acceptance, not proof of remote TCP receipt. The caller must return
the response concurrently with pumping, provide a wakeable request/grant control
and persist delivered reports; no public runtime caller or account supplier is installed.

`prepare_owned` uses the same checked response plan as `prepare`, but retains
an immutable `Rc<OriginalSource>`. It accepts either a validated source value
(allocated once) or the supplier's existing request-local `Rc` (no deep clone or
second lookup). Its `OwnedOriginalPlay::pump` constructs the single shared reader
inside the enclosing task. `pump_owned` also retains the transport, request
control and delivered sink, installing the writer abort guard **before** the
scheduler wraps the future. Even an unpolled task drop therefore aborts the
writer with no article/write/close I/O or interval. Both entry points use the
same pump and acknowledged-prefix settlement; expiry is rechecked on first
output work after scheduling.

Shared source ownership does not make borrowed pool inputs static. The existing
pool owner must still supply owned registry/accounts/channel keys/clock/signals
and operation timer, retaining the same request authority through task cleanup.
Synthetic retained-input fixtures cover source-allocation retention, refusal
release, unpolled drop and pending article/write/close drop/cancellation with one
ACK-prefix interval. They are not production pool-task or actual Worker proof.

The off-route wasm `schedule_response` creates the fixed-length channel, moves
the source/supplier/control/writer into a `Context::wait_until` task, and returns
the response without awaiting the backpressured pump. Its `DeliveredSink`
forwards the one settled interval through a separate `wait_until` call to the
existing runtime reporter and bounded retries; it adds no meter, retry policy,
wire format or account-release action. Source lookup and account construction
remain with their existing owners. The scheduler accepts the existing
`OriginalControl` trait rather than requiring a separate concrete control. A
production supplier and response must share the same checked request authority
through thin forwarding; do not create a second clock watermark or cancellation
authority during composition. The off-route account supplier above supplies that
forwarding; it does not install a runtime caller. Nothing calls this scheduler
from the public fetch path. Runtime task limits, nested
report scheduling and disconnect/report
durability still require actual Worker qualification; static compilation and
owned-task fixtures do not establish them.

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
