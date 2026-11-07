# Source-only generated Worker qualification

The required `worker-preparation-qualification` Actions job uses the canonical
[toolchain manifest](../../../.github/worker-toolchain.json). Effective build
adoption must read that same manifest rather than introduce a competing pin.
No deploy workflow, Wrangler main/build command, binding grant or runtime mode
is changed. The API bridge is built only into this job's qualification artifact.

Both actual release Workers and a separate synthetic-producer qualification
artifact are generated using the selected builder and all three verified tool
overrides, including wasm-opt. Any missing override, fallback
download, nonempty builder download cache, lock drift or version mismatch fails
the job. The builder and bindgen crate archives are checked against both the
selected SHA-256 and registry index; the Binaryen archive is checked against the
selected SHA-256 and its published checksum. The isolated pnpm importer has a
script-disabled frozen install. See [supply-chain limitations](SUPPLY_CHAIN.md).

The Node test imports Miniflare from this package only. Actual generated JS/Wasm
and `cloudflare:workers` execute inside workerd, without VM substitutions. The
harness-only roots import the unchanged facade and generated Workers; they are
not effective application entrypoints. Snapshot assertions cover the actual
export namespaces and class prototypes, including the locked SDK's three data
classes emitted by the builder's class wrappers. The facade must export none
of those classes or the raw private bridge; it retains only its two composed
entrypoints. Both Workers' snapshots are compared together so a failure reports
the full ABI delta rather than stopping at the first namespace mismatch.
Real service bindings exercise the
named facade, mode refusal, a redacted injected bridge failure, and public
surface isolation for the tested request matrix. The generated API runs with
no keyring/config and must refuse startup; ingest's health fetch runs normally.
The harness denies outbound fetches and uses no real credentials.

A real local R2 bucket and Queue producer/consumer are composed with ingest.
The harness writes a synthetic three-byte object and sends an opaque pointer;
the generated Rust consumer receives it and encounters an unknown recovery
outcome without DB prerequisites. A completion marker records callback return;
a separate real dead-letter Queue observer must receive the exact pointer,
proving native failure settlement rather than a successful ack. R2 readback
checks that the object remains. This is **platform composition and
failed-recovery retention**, not a test of the Rust tagged-object producer:
the seed write/send are harness JavaScript. It does not establish successful
DB-backed prepare/freeze/open, authenticated admission, the actual Rust R2
create-only/range adapter, process-crash/ambiguous-commit recovery or deployment
lifecycle acceptance. The separate composite control below addresses some of
those gaps; neither control qualifies the whole foundation by itself.

The fault probe replaces only the generated private method in its own test
isolate, preserving the real generated constructor and platform lifecycle. It
is an explicit fault injection, not production logic or successful DB evidence.
The original public matrix runs without API configuration. A separate isolate
now gets ephemeral harness-generated session/KEK inputs and must serve the real
Rust health route before and after its matrix. The harness constructs the
application Origin inside workerd and forwards through a service binding to the
unchanged public/default facade. This avoids the Node binding proxy's separate
nonlocal-Origin rejection; no runtime host guard is disabled. Health must also
return the exact configured CORS origin, proving the Rust router received that
header. That matrix covers four paths across seven methods, despite the Origin,
mode header, query hint and binary body: 27 exact 404s plus the existing generic
empty 204 for `OPTIONS /v1/ingest/preparation`. The scope map already covers
`POST /v1/ingest/*`, so browser preflight answers that namespace even where no
handler exists. The test pins its exact method/header grants and empty body;
the 404 cases must omit those grants. No application routing or security guard
is changed to make the harness pass. Every matrix response must carry the exact
CORS origin and a valid Rust trace ID, not a harness/proxy response. Per-request
bridge counts stay unchanged, including on preflight. A named-binding
positive control must reach the injected failure and increment the same count,
while its response remains redacted and no-store. No generated/platform class
is replaced. These configured routes do not need a DB; successful DB-backed
private protocol, Rust producer and composite recovery acceptance remain open.
This is not a proof over every configured API route or credential posture.

## Rust producer and database-backed composite control

The ordinary ingest artifact is preserved separately and its exact prototype
must omit both qualification bridges. Only a third, Actions-only build selects
`preparation-qualification`; no effective Wrangler build selects that feature.
Its raw methods have no public router entry, accept no caller-selected producer
identity and call the existing `prepare_and_send` and `recover_delivery` directly.
The fixed tagged-output fixture crosses the 64-KiB reader boundary, but is not a
real FLAC parser/tagger result. These are test bridges, not a business entrypoint.

A disposable loopback PostgreSQL service receives the full migration/replay/SQL
bank. The harness refuses a missing or non-Actions/non-loopback/non-test-database
DSN. It seeds shape-valid historical admission custody, not an authenticated
HTTP upload or genuine original envelope. The actual Rust producer must create
its tagged R2 object, canonical plan, encrypted API-keyring DB envelope, dispatch,
receipt and attempt, then send through the real Queue producer. The plan service
is the unchanged named facade in a separate generated API isolate with ephemeral
KEK/session inputs and its own Hyperdrive binding. No JS plan freeze/open double,
R2 write or Queue send supplies the successful producer path.

The control requires exact object version/bytes/byte-derived digest, one sealed
plan/attempt/receipt, acknowledged send, successful Rust pointer recovery in the
real Queue callback and unchanged full-row DB fingerprints across recovery and
producer replay. R2 must contain no plan object. Unknown pointers and replacement
of the tagged object with a new version must refuse without DB changes or R2
deletion. The real generated Queue consumer still owns native ack/retry; an extra
read-only Rust recovery observes success before it, a marker observes callback
return, and any observed dead-letter delivery vetoes the positive control. This
is not direct instrumentation of the native ack call or an unbounded no-retry
proof. Ordinary failed-recovery/native-DLQ acceptance remains separate above.

The first composite execution reached the actual Rust R2 reader, then panicked
at the locked SDK's class-typed `Object::range` getter: workerd returns a plain
range dictionary, not an exported Wasm `R2Range` instance. The adapter now uses
the locked raw bucket binding and checks the response dictionary before bounded
body streaming. Exact key/version, numeric offset/length, absent suffix, valid
size and exact streamed length remain required. Harness-only GET response faults
cover offset, length, type, suffix, null range, version, non-finite size and short/
long bodies. Each must refuse before DB dispatch while retaining the R2 object;
the unchanged real multi-range producer/recovery path is the positive control.
No runtime/provider guard or test expectation is relaxed to hide the panic.

At head `bd7ff8df`, [exact-head qualification Actions](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42377491568/job/128418003741)
passes all eight runtime subtests (nine Node passes including the enclosing test).
This includes the corrected range faults and actual Rust composite above, not
just the earlier JS-seeded controls.

A separate synthetic structural FLAC control now feeds the actual `rewrite_tags`
parser/tagger before the same Rust preparation/R2/private plan service/DB/Queue
path. Fixed harness tags and producer identity cannot be chosen by the request.
Malformed magic, frame CRC, total samples and an oversized harness body must
refuse before R2 or DB writes. The positive fixture crosses the range boundary;
assertions require stripped padding, fresh comments, unchanged frame bytes,
byte-derived tagged digest distinct from the input, exact R2 version, sealed plan,
Queue recovery and immutable producer replay. Separate fixed owner/job/operation
custody and an isolated bucket keep the earlier control's rows and deliberately
replaced object from satisfying or blocking this one. The original-upload hash
must also be distinct: the admin-job unique index is global, not per owner. Both
seeded jobs must coexist with different hashes, with each admission key and original
custody matching its own upload identity. The first structural-control Actions run
failed at that seed index before parsing; it is not parser/tagger acceptance.
The fixture is generated structural data: no PCM decoder or MD5 verification is
performed, and historical seeded admission is still not authenticated HTTP upload.
At head `524e39f9`, [exact-head qualification Actions](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42381117815/job/128428780077)
passes all nine runtime subtests (ten Node passes including the enclosing test),
including this structural FLAC composition and all four malformed-input controls.
The same head's [database job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42381117815/job/128428780004)
explicitly reports 255 required cases passed. These are exact-head Actions results,
not authenticated-upload, PCM-decoder, security, staging or release acceptance.

An additional lifecycle control now disposes the entire Miniflare/workerd runtime
and creates a fresh one using the same temporary R2 persistence, external DB and
ephemeral API keyring. No object, plan or admission is reseeded. Fresh Rust and API
isolates must recover the stored FLAC pointer, preserve the exact R2 version/digest
and full DB fingerprints, reject an unknown pointer, and replay the producer
without reminting or renewing authority. Each replay removes only the old harness
completion marker and waits for a fresh callback marker before checking DB/DLQ
state; pre-restart replay completes first so it cannot satisfy the restart check.
The converter must retain the selected `resourcePersistencePath`. Temporary
persistence is removed only after runtime disposal. At head `659aafbd`,
[exact-head qualification Actions](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42383178630/job/128434934300)
passes all ten runtime subtests (eleven Node passes including the enclosing test),
including graceful runtime replacement. The same head's
[database job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42383178630/job/128434934560)
explicitly reports 255 required cases passed. Graceful restart is not abrupt crash,
ambiguous-commit or pending-Queue replay injection.

A private-response control opens the real stored plan through the actual service
binding before injecting status, redirect, content-type, digest, short-body,
stream-error, oversized-body and stalled-body responses into read-only recovery.
The oversize stream exceeds the existing 1-MiB bound and stays open: refusal must
precede the existing ten-second operation deadline, not depend on EOF or digest
rejection. A one-byte stalled stream must instead reach that deadline. Every
control requires exactly one successful real plan call, a redacted refusal,
unchanged full DB fingerprints and exact R2 version/digest, no new objects or
Queue marker, and successful genuine recovery afterward. At head `b95b392f`,
[exact-head qualification Actions](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42385014133/job/128440474769)
passes all eleven runtime subtests (twelve Node passes including the enclosing
test), including these response controls. The same head's
[database job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42385014133/job/128440475247)
explicitly reports 255 required cases passed. Workerd emits critical-error
messages during injected stream failure; the refusal, unchanged-state and later
genuine recovery assertions pass. This is not clean runtime diagnostics. The
digest fault changes only the original-digest field, preserving canonical format
and geometry; this exercises end-to-end integrity, not one particular duplicate
digest check. Truncation does not isolate length from digest/decoder refusal.
The synthetic redirect is returned after the actual service fetch, so it proves
status refusal, not platform redirect-following prevention. No production bound
or timeout is shortened to make the tests pass. This covers
bounded private response handling, not maximum supported media-file sizing,
network cancellation, a process crash or ambiguous commit. Process crashes,
ambiguous DB commits, pending-Queue replay, large-media limits, deployed
lifecycle/drain, actual staging/restore and remaining foundation review stay open.
The FLAC composite now also faults the native Queue send before acceptance and
after a successful real send (lost reply). Each producer call must reach exactly
one send attempt and return a static refusal while the persisted dispatch stays
unacknowledged. Direct recovery must work before any send; after the lost reply,
a fresh callback marker must prove actual Rust Queue recovery. Full DB
fingerprints must remain unchanged across both faults. An ordinary producer retry
then sets the acknowledgement bit, changes no other persisted field, preserves
the exact R2 version and requires another fresh callback marker. No DB reset,
manual acknowledgement or replacement custody is used. At head `474aabaa`,
[exact-head qualification Actions](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42386967887/job/128446288295)
passes all eleven runtime subtests (twelve Node passes including the enclosing
test), including these Queue controls. The same head's
[database job](https://github.com/TogetherWeOwn/nntunev2/actions/runs/42386967887/job/128446288347)
explicitly reports 255 required cases passed. The post-send throw simulates a lost
Queue reply, not a process crash, ambiguous DB commit or pending Queue persistence
across restart.

A separate R2 recovery control retains the actual GET metadata and faults only
its body: an open oversized chunk, two individually bounded chunks whose total
exceeds the requested range, a stream error and a stalled prefix. All must reach
exactly one real GET and return the static redacted refusal. Oversize/error
refusals must precede the existing ten-second operation deadline; the stalled
body must instead reach it. Full DB rows and every R2 key/version must stay
unchanged, exact tagged bytes/digest must survive, no Queue marker may change,
and genuine recovery must succeed after each fault. These controls are written,
not locally executed; require exact-head Actions. They qualify bounded range
stream handling, not maximum-media sizing, network cancellation or abrupt crash.

No test uses candidate media keys, articles or manifests.
Passing this job is not security review, staging/restore proof, production
activation, posting or R2 deletion authority.

Do not run this job as a local CI mirror. Full builds, clean install and runtime
tests run on GitHub Actions for the exact PR head. A skipped or zero-test run is
not acceptance. New test controls are unexecuted until that evidence is recorded.
