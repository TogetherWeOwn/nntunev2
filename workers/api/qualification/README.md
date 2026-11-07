# Source-only generated Worker qualification

The required `worker-preparation-qualification` Actions job uses the canonical
[toolchain manifest](../../../.github/worker-toolchain.json). Effective build
adoption must read that same manifest rather than introduce a competing pin.
No deploy workflow, Wrangler main/build command, binding grant or runtime mode
is changed. The API bridge is built only into this job's qualification artifact.

Both actual release Workers are generated using the selected builder and all
three verified tool overrides, including wasm-opt. Any missing override, fallback
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
lifecycle acceptance. Those acceptance gaps remain open; this job does not
qualify the whole preparation foundation by itself.

The fault probe replaces only the generated private method in its own test
isolate, preserving the real generated constructor and platform lifecycle. It
is an explicit fault injection, not production logic or successful DB evidence.
The original public matrix runs without API configuration. A separate isolate
now gets ephemeral harness-generated session/KEK inputs and must serve the real
Rust health route before and after its matrix. That matrix requires exact 404s
for four unserved paths across seven methods, despite a mode header, query hint
and binary body. Per-request bridge counts stay unchanged. A named-binding
positive control must reach the injected failure and increment the same count,
while its response remains redacted and no-store. No generated/platform class
is replaced. These configured routes do not need a DB; successful DB-backed
private protocol, Rust producer and composite recovery acceptance remain open.
This is not a proof over every configured API route or credential posture.
No test uses candidate media keys, articles or manifests.
Passing this job is not security review, staging/restore proof, production
activation, posting or R2 deletion authority.

Do not run this job as a local CI mirror. Full builds, clean install and runtime
tests run on GitHub Actions for the exact PR head. A skipped or zero-test run is
not acceptance. New test controls are unexecuted until that evidence is recorded.
