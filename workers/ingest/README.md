# workers/ingest — ingest pipeline (slices 1–2: contracts + Queue wiring)

User-scoped ingest jobs via Queues/Workflows:

```text
Indexer search (BYO key) ─┐
                          ├─→ parse (nntune-core) → pool-DO probe → par2 verify ─┬─→ repair (Container batch)
NZB upload ───────────────┘                                                      └─→ tag/fingerprint + MB match (workers/mb) → library rows
```

## This slice

- `GET /healthz`: unauthenticated Worker-only health check returning `{"ok":true}` with `Cache-Control: no-store`; it does not access bindings.
- Job state machine (`src/jobs.rs`): `Queued → Searching|Parsing →
  Probing → Verifying → (Repairing → Verifying)? → Matching → Writing →
  Done`, `Failed` terminal. Checkpoints make redelivery/replay converge;
  idempotency keys bind user + content sha + source ref (`:`-free fields, so
  triples cannot collide). Repair rounds are budgeted (`MAX_REPAIR_ROUNDS`):
  a perpetually failing verify ends in `Failed`, never in a loop.
- Newznab search builder (`src/newznab.rs`): `t=music`, `https://` only,
  no embedded credentials. The URL is built **without** the key; the
  vault-resolved key is injected at request time and redacted in logs.
- NZB intake guard (`src/nzb_intake.rs`): versioned caps
  (`INTAKE_CONFIG_VERSION` v1: 2 MiB / 5k files / 60k segments / 64 GiB
  declared segment bytes) plus well-formedness validation before anything
  spends provider connections. Refusals use the shared API error envelope:
  `code` is `invalid_request` or `payload_too_large`, while the stable
  intake-specific identifier (`NZB_EMPTY`, `NZB_TOO_LARGE`, `NZB_NOT_NZB`,
  `NZB_MALFORMED`, `NZB_TOO_MANY_FILES`, `NZB_TOO_MANY_SEGMENTS`, or
  `NZB_DECLARED_BYTES_OVER_CAP`) is in `intake_code`. The response also keeps
  `error` as an alias of `message` and includes `intake_config_version`; cap
  refusals return measured values and caps (`file_count`/`max_files`,
  `segment_count`/`max_segments`, `declared_bytes`/`max_declared_bytes`, …)
  so clients can render a reason.
- Per-plan rate limits (`src/rate_limit.rs`): provisional base/5x/20x
  tunables (exact numbers are set at launch); token buckets, no clock
  dependency.
- Queue/Workflow payloads (`src/queue.rs`): `QueueMsg`, `RepairRequest`,
  `LibraryWrite` — all carry the idempotency key. MB calls target the
  `workers/mb` read API (`/v1/mb/release/:mbid`, `/v1/mb/recording/:mbid`,
  `/v1/mb/release/:mbid/recordings`, `/v1/mb/search?type=…`).
- Routes (`src/lib.rs`): `POST /v1/ingest/search` (16 KiB body cap,
  ≤16 categories, paged window), `POST /v1/ingest/nzb` (wire cap accounting
  for JSON escaping, then the versioned intake guard) — malformed JSON is
  400, intake refusals use the registered API code plus `intake_code` (bad
  input 400, over-cap 413 with measured + cap fields), busy store/queue is
  503; success is 202 + enqueue with job id + idempotency key. NZB uploads
  are also parsed by the shared bounded `nntune-core` parser and
  cross-checked against the intake guard before enqueue (parser rejection
  400, parser bounds 413). Every real admission writes the initial job
  checkpoint (`src/checkpoint.rs`), resetting a stale `Failed` checkpoint
  on re-admission, so `GET /v1/ingest/jobs/:id` reads durable status from
  `INGEST_STATE` (404 unknown, 503 store failure). Re-posting a job that
  already failed inside the admission replay window is refused with 409
  `conflict` (nothing is enqueued then either; the failure stays readable
  via the job status).

## Security rules

- `indexer_ref` is an opaque vault handle. Raw indexer/provider keys are
  never accepted in API bodies, never logged (redacted URLs only), never
  committed. `http://` indexer URLs are refused.
- Upload bodies are untrusted: capped, shape-checked, never logged.
- Outbound indexer fetch: `src/indexer_fetch.rs` is a **policy/model
  library, not a live SSRF boundary** (see next section; module at
  `src/indexer_fetch.rs:1-42`, exported as `pub mod indexer_fetch` in
  `src/lib.rs:34`). No ingest route
  or queue step calls it, no live resolver or HTTPS transport implements
  its traits in this tree, and no HTTP response or decision log emitted by
  a running Worker comes from it. Its fake-DNS/fake-transport tests prove
  model behavior only (`tests/indexer_fetch_ssrf_guard.rs`). The separate `workers/api` vault validator
  (`IndexerConfig::validate` in `workers/api/src/vault.rs:376`) still checks the indexer URL at
  save/validate time; it is not an indexer fetch transport either.

## Indexer fetch policy library — no runtime indexer fetching

`src/indexer_fetch.rs` is a **library-only policy guard and buffered
model**, not delivered outbound fetching. It is exported as
`pub mod indexer_fetch` (`src/lib.rs:34`), but nothing outside its unit tests
(`src/indexer_fetch.rs:526-677`) and
`tests/indexer_fetch_ssrf_guard.rs` calls `fetch_indexer`
(`src/indexer_fetch.rs:423`) or
`validate_indexer_url` (`src/indexer_fetch.rs:330`): there is no route, queue-consumer or search-step
call site, no non-test `DnsResolver` (`src/indexer_fetch.rs:46-48`) or `IndexerTransport`
(`src/indexer_fetch.rs:153-155`) implementation
for this module, no live DNS/TCP/TLS dial, no cancellation, no emitted
HTTP response and no emitted decision log. Existing search and NZB upload
routes are unchanged.

### What the model checks

- `validate_indexer_url` (`src/indexer_fetch.rs:330`) checks https only
  (`split_authority`, `src/indexer_fetch.rs:245`), no embedded credentials,
  and a 443-only port allowlist (`ALLOWED_INDEXER_PORT = 443`,
  `src/indexer_fetch.rs:36`; `split_host_port`/`check_port`,
  `src/indexer_fetch.rs:267-322` — any other explicit port is refused in the
  model before the transport trait would be called, so the modeled fetch
  cannot be aimed at another port as a scan primitive). Address ranges
  mirror the `workers/api` vault guard (`v4_is_forbidden`/`ip_is_forbidden`
  identical in `workers/api/src/vault.rs:196-227` and
  `src/indexer_fetch.rs:206-237`) — keep the two lists in sync.
- Every answer from the supplied `DnsResolver` is checked for forbidden
  ranges (`ip_is_forbidden`, `src/indexer_fetch.rs:221`; one forbidden
  record poisons the whole answer, `src/indexer_fetch.rs:360-363`):
  loopback / link-local (incl. the cloud metadata endpoint) /
  private / unique-local / CGNAT / multicast / broadcast / documentation
  / reserved space (v4+v6, incl. IPv6 transition space with a
  public-looking payload); one forbidden record poisons the whole answer.
  IP literals skip DNS, not policy (`src/indexer_fetch.rs:351-354`).
- The intended DNS-pin contract shape: `fetch_indexer` validates each hop
  before calling the transport and hands it a `PinnedRequest`
  (`src/indexer_fetch.rs:140-147`) carrying
  the vetted addresses, the original hostname as `sni_host` (for TLS
  SNI/certificate checks and the Host header), port 443 and
  `dial_timeout_ms` (`src/indexer_fetch.rs:434-442`). In the model the host is resolved exactly once per
  hop, so a changed second DNS answer (rebinding) is never consulted by
  `fetch_indexer` itself.
- Every redirect hop (301/302/303/307/308 per `is_redirect`,
  `src/indexer_fetch.rs:404-406`; resolved per `resolve_location`,
  `src/indexer_fetch.rs:379` — absolute / protocol-relative /
  path-absolute) is re-validated from scratch and at most 3 hops are
  followed (`MAX_REDIRECTS = 3`, `src/indexer_fetch.rs:42`; cap enforced at
  `src/indexer_fetch.rs:469-474`); the original URL's `apikey=` value is not re-injected onto a
  redirect target. See the unresolved redirect-policy discrepancy below.
- Modeled deadlines: a supplied `connect_delay_ms` reaching the 5000 ms
  dial timeout (`DIAL_TIMEOUT_MS`, `src/indexer_fetch.rs:38`; enforced with
  `>=` at `src/indexer_fetch.rs:444-446`) returns the dial-timeout rejection, and supplied connect +
  chunk delays accumulated across all hops reaching the 15000 ms total
  timeout (`FETCH_TIMEOUT_MS`, `src/indexer_fetch.rs:40`; enforced with `>=`
  at `src/indexer_fetch.rs:447-456`)
  (the vault outbound numbers, reused) return the total-timeout
  rejection; at the deadline counts as expired, and partial bytes are not
  returned on a model rejection.
- `url_host_for_log` (`src/indexer_fetch.rs:484`) extracts a sanitized host for a future host + code
  decision log. Stable codes (`FetchRejection::code`,
  `src/indexer_fetch.rs:86-99`) and their `http_status()` mappings
  (`src/indexer_fetch.rs:102-113`: guard
  blocks 4xx, timeouts 504, upstream failures 502) are library values for
  the shared taxonomy envelope, not current HTTP responses:
  `indexer_fetch_bad_scheme`, `indexer_fetch_malformed_url`,
  `indexer_fetch_embedded_credentials`, `indexer_fetch_port_not_allowed`,
  `indexer_fetch_forbidden_address`, `indexer_fetch_unresolvable_host`,
  `indexer_fetch_too_many_redirects`, `indexer_fetch_dial_timeout`,
  `indexer_fetch_timeout`, `indexer_fetch_upstream_status`. Full URLs
  must go through `newznab::redact_api_key` (`src/newznab.rs:144`) before any future log line;
  no such log is emitted today.

### Limits of the model

`IndexerTransport::get(&PinnedRequest)` (`src/indexer_fetch.rs:153-155`)
is synchronous and returns a
fully buffered `RawResponse` (`src/indexer_fetch.rs:159-164`) that already contains every body chunk.
The delay checks run **after** that call returns (`src/indexer_fetch.rs:443-456`), on simulated/reported
`connect_delay_ms` and chunk `delay_ms` values — they are not wall-clock
measurements. A hung transport cannot be cancelled, DNS/connect/TLS/header
durations are not measured, and the body is assembled from the buffered
chunks with no streaming bound. Whether a real transport would actually
dial only `PinnedRequest.addresses` on 443, never re-resolve, and verify
TLS against `sni_host` is a property of a future implementation, not
something this module enforces or its tests prove.

The fake DNS/transport tests therefore prove policy/model behavior only:
URL/port/range rejection, one-resolve-per-hop ordering inside
`fetch_indexer`, redirect re-validation with the model cap, and
deadline arithmetic over supplied delays. Passing
`indexer_fetch_ssrf_guard` is not evidence of runtime pinning, TLS/DNS
enforcement, wall-clock cancellation, streamed byte bounds, route or
queue wiring, emitted HTTP statuses, log emission, or a safe live-network
or deployment boundary.

### Unresolved: redirect policy

The required security policy still forbids redirects for indexer fetch:
`docs/security/threat-model.md` (F-INGEST-01, `docs/security/threat-model.md:61`) requires forbidden
redirects, and the vault side encodes `INDEXER_ALLOW_REDIRECTS = false`
(`workers/api/src/vault.rs:47`).
This model instead follows up to 3 re-validated redirects
(`MAX_REDIRECTS = 3`, `src/indexer_fetch.rs:42`). That discrepancy is **unresolved**: this README
does not relax the required policy, does not approve live redirect
following, and does not authorize deployment of an indexer fetch
transport. Changing the required policy needs an independent CISO
disposition; until then the model's redirect-following behavior must not
be wired to a live transport as if it were approved policy.

## NZB URL policy library — no runtime URL fetching

`src/nzb_fetch.rs` is a **library-only policy guard and buffered simulator**,
not a live SSRF boundary (module at `src/nzb_fetch.rs:1-68`, exported as
`pub mod nzb_fetch` in `src/lib.rs:40`). No ingest route calls it
(`fetch_nzb` at `src/nzb_fetch.rs:436` has no non-test call site; only
`tests/nzb_fetch_ssrf_guard.rs` calls it); no runtime resolver or
HTTPS transport is provided (no non-test `FetchTransport`
`src/nzb_fetch.rs:192-194` implementation exists), `NZB_FETCH_ALLOWLIST` is not read from the
Worker environment (`allowlist_from_env` takes a caller-supplied string,
`src/nzb_fetch.rs:114-121`), and no block logs are emitted. Exporting `pub mod
nzb_fetch` does not wire a fetch-from-URL feature. Existing search and NZB
upload routes are unchanged.

### What the library checks

- `validate_fetch_url` (`src/nzb_fetch.rs:338`) checks HTTPS
  (`split_authority`, `src/nzb_fetch.rs:253`), no embedded credentials,
  default/443 port (`ALLOWED_FETCH_PORT = 443`, `src/nzb_fetch.rs:81`;
  `split_host`/`check_port`, `src/nzb_fetch.rs:276-321` — any other explicit
  port is `MalformedUrl`, never a fetch),
  and exact case-insensitive host allowlisting (`host_allowlisted`,
  `src/nzb_fetch.rs:325-330`; subdomains are not
  implied). `FetchConfig::default()` denies every host
  (`src/nzb_fetch.rs:99-108`). The parser accepts
  a supplied comma-separated string such as `nzb.example,cdn.example`
  (`allowlist_from_env`, `src/nzb_fetch.rs:114-121`); it
  does not load environment configuration.
- Every answer from the supplied resolver is checked for forbidden ranges
  (`ip_is_forbidden`, `src/nzb_fetch.rs:230`; one forbidden record poisons
  the whole answer, `src/nzb_fetch.rs:368-372`;
  guard mirrors the `workers/api` vault guard,
  `workers/api/src/vault.rs:196-227` — keep the two lists in sync)
  (loopback, link-local/metadata, private, CGNAT, multicast, reserved and
  documentation, including IPv6 transition space). IP literals skip DNS,
  not policy (`src/nzb_fetch.rs:359-362`). The result records vetted addresses
  (`ValidatedUrl`, `src/nzb_fetch.rs:178-186`), not a network binding.
- `fetch_nzb` (`src/nzb_fetch.rs:436`) models per-hop validation and at most
  3 redirects (`MAX_REDIRECTS = 3`, `src/nzb_fetch.rs:77`; cap enforced at
  `src/nzb_fetch.rs:471-476`; 301/302/303/307/308 per `is_redirect`,
  `src/nzb_fetch.rs:416-418`). It checks
  accumulated reported chunk delays across hops against 10000 ms
  (`FETCH_TIMEOUT_MS`, `src/nzb_fetch.rs:75`; enforced with `>=` at
  `src/nzb_fetch.rs:450-455`) and the
  final returned body against the 2 MiB intake cap (`MAX_NZB_BYTES`,
  `src/nzb_intake.rs:42`, via `FetchConfig.max_bytes`,
  `src/nzb_fetch.rs:96`; enforced at `src/nzb_fetch.rs:460-464`),
  returning stable
  rejection reasons without partial results.
- `url_host_for_log` (`src/nzb_fetch.rs:490`) extracts a sanitized host. Stable codes/status mappings
  (`FetchRejection::code`, `src/nzb_fetch.rs:141-154`;
  `http_status`, `src/nzb_fetch.rs:157-170`)
  are library values for future route wiring, not current HTTP responses.

### Limits of the model

`FetchTransport::get(&str)` (`src/nzb_fetch.rs:192-194`) receives only a URL, **not the validated
addresses/hostname or a remaining deadline** (compare the call at
`src/nzb_fetch.rs:449`, which passes only `&current`, never the
`ValidatedUrl` addresses/host). It cannot enforce IP-pinned
dialing, original-host TLS verification or DNS-rebinding resistance. A
`RawResponse` (`src/nzb_fetch.rs:199-203`) already contains all body chunks: size and reported-delay
checks happen **after buffering** (`src/nzb_fetch.rs:450-464`), not while streaming. DNS, connection,
TLS and header durations are not measured; a hung transport cannot be
cancelled. Do not attach a real HTTPS client to this simulator interface.

The fake DNS/transport tests prove policy behavior only. The original
runtime-wiring prose in `tests/nzb_fetch_ssrf_guard.rs` is the future end
state, not delivered functionality; that test file is unchanged. Passing it
is not evidence of runtime pinning, deadlines, streamed byte bounds,
route/environment wiring or log emission.

### Tracked runtime follow-up

[Safe pinned HTTPS transport and ingest URL route (#84)](https://github.com/TogetherWeOwn/nntunev2/issues/84)
owns the remaining work: a transport receiving the validated address/host
set with original-host SNI/certificate/Host validation and no re-resolution;
cancellable streaming with one deadline across DNS/connect/TLS/headers/body
and all redirects; bounded buffering; authenticated route wiring with a
deny-all `NZB_FETCH_ALLOWLIST`; and host + stable code-only rejection logs.
The follow-up also owns acceptance tests proving those live properties.
Keep URL fetching unavailable until a supported transport demonstrates them.

Stable codes: `nzb_fetch_bad_scheme`, `nzb_fetch_malformed_url`,
`nzb_fetch_embedded_credentials`, `nzb_fetch_host_not_allowlisted`,
`nzb_fetch_forbidden_address`, `nzb_fetch_unresolvable_host`,
`nzb_fetch_too_many_redirects`, `nzb_fetch_timeout`, `nzb_fetch_too_large`,
`nzb_fetch_upstream_status`. HTTP mappings are documented on the module's
`FetchRejection` type (`http_status`, `src/nzb_fetch.rs:157-170`: 400
scheme/malformed/credentials/unresolvable/redirect-cap, 403
not-allowlisted/forbidden-address, 504 timeout, 413 too-large, 502
upstream-status); no route emits them in this slice.

## Synthetic NNTP CI coverage

`tests/synthetic_nntp.rs` runs the shared `nntune-core` NZB parser and
NNTP client against the merged `nntune-test-nntp` fixture server, fetches
the parsed Message-ID, and verifies shared yEnc decoding (no real Usenet
in CI). This validates parser and server interoperability only — not the
Worker bindings, Newznab provider, pool DO, repair Container, or
end-to-end library write path. Still wanted from the synthetic corpus:
obfuscated subjects, slow-link/mid-disconnect modes for the resume test
(kill mid-`Probing`, redeliver, expect one library row), and a Newznab
stub whose NZB references fixture message-ids.

## Slice 2 (this slice)

- Queue wiring (`src/dispatch.rs`): routes build a `QueueMsg` carrying the
  full job input (search params, or a content-addressed `INGEST_PAYLOADS`
  handle for upload bytes written before enqueue) with a user-bound
  idempotency key, and `send` it to `nntune-ingest-jobs` (`worker` `queue`
  feature; bindings in `wrangler.toml.example`). Search keys bind query +
  indexer + categories + limit + page, so no request is silently dropped.
  Success is 202 `queued`; a missing/failing binding or payload store is 503
  with `Retry-After` (never 202). The Queue handler exists
  (`src/queue_consumer.rs`: per-message identity validation, exactly one
  planned checkpoint write per delivery — Workers KV allows 1 write/s per
  key, so a persist colliding with the admission route's write to the same
  key waits out the window and retries that write once in-invocation —
  `retry::decide_checkpoint` settlement, DLQ quarantine that never acks on
  a failed send, shared-parser upload validation, and the pool-DO segment
  probe for uploads), but the consumer/workflow entries stay commented
  out in `wrangler.toml.example`: the search/verify/repair steps are not
  wired yet, so enabling the consumer now would burn every valid job's
  retry budget against an unavailable boundary and quarantine it.
- Request bodies carry self-asserted `user_id` (1..=128 chars) until the
  accounts card authenticates callers; the key format already binds the
  user, so keys stay valid when auth lands.
- Hyperdrive library upsert (`src/library.rs`): one row per idempotency
  key, `ON CONFLICT DO UPDATE` so replays converge on the latest MB match.
- Repair dispatch (`src/repair.rs`): missing blocks + par2 set → Container
  `POST /repair`; missing blocks with no par2 set fails the job.
- Repair re-verify guard (status only — no runtime change): the re-verify
  feedback loop is still open. After a repair round, `Verifying` re-runs
  the verify driver over the pool body route (`src/queue_consumer.rs:696-699`,
  `PoolBodySource::fetch_body` via `call_pool_body` at
  `src/queue_consumer.rs:792-799`) and does not yet consume the repair
  round's `out/` objects (the repair Worker streams outputs back to `out/`
  per `workers/repair/README.md:23-28`; key shapes in
  `workers/repair/src/contract.rs:278-286`), so the `Repairing` step must
  not be exercised live until that consumption lands
  (`src/queue_consumer.rs:245-253`). The consumer stays disabled meanwhile:
  the `[[queues.consumers]]` entry stays commented out
  (`workers/ingest/wrangler.toml.example:30-31`,
  `workers/ingest/wrangler.toml.example:58-59`), and ingest stays unable
  to finish a `Repairing` job (`workers/repair/README.md:40-44`) — this
  is one gate alongside the search step, which is also not wired yet, so
  enabling the consumer now would burn search jobs' retry budget
  (`workers/ingest/wrangler.toml.example:52-56`).
  A checkpoint reaching `Repairing` without a recorded `par2_set_id` is a
  permanent failure — there is nothing a redelivery could add
  (`src/queue_consumer.rs:251-253`; the missing-inputs arm at
  `src/queue_consumer.rs:257-262`); with `missing_blocks == 0` (the
  `#[serde(default)]` zero at `src/jobs.rs:78-81`) the step takes the
  `Ok(None)` arm (`src/repair.rs:43-45`,
  `src/queue_consumer.rs:272-278`) and returns to `Verifying`. The ingest
  Worker owns repair dispatch and queue behavior (`src/repair.rs`,
  `src/queue_consumer.rs`); the repair Worker owns the Container runtime,
  whose contract is `workers/repair/README.md`; this note only records
  the guard.
- Metering (`src/metering.rs`): one `nntune_ingest_meter` event per
  workflow step (workers-rs 0.3 has no Analytics Engine client, so the edge
  write goes through the dataset binding's HTTP API).
- Probe envelope + resume mapping (`src/probe.rs`), parse shape +
  intake cross-check + probe batching (`src/parse.rs`), synthetic fixtures
  (`src/fixtures.rs`), pipeline E2E (`tests/pipeline_e2e.rs`): happy path
  reaches `Done` with one library row; the gapped set repairs then finishes.
- Consumer pool probe (`src/queue_consumer.rs`, client in
  `src/pool_probe.rs`): an upload in `Probing` re-parses its
  content-addressed payload, asks the user's `NntpPool` DO about
  `parse::probe_batches` batches not yet in the checkpoint tally
  (`build_probe_request_with_ranges` [`src/probe.rs`] →
  `call_pool_probe` [`src/pool_probe.rs`] → `apply_probe_batch`
  [`src/probe.rs`]), and moves to `Verifying` carrying the completed
  tally's missing-segment count. Folded batches ride the delivery's one
  checkpoint write (`plan_settlement`/`persist` in
  `src/queue_consumer.rs`; KV allows 1 write/s per key per
  `KV_SAME_KEY_WRITE_WINDOW_MS = 1_000` in `src/checkpoint.rs`, so there
  is deliberately no write per batch); a crash before that write re-asks
  them, which is safe because `STAT` is idempotent and the tally is keyed
  by batch index (`ProbeTally` in `src/probe.rs`). Probing is
  budget-capped per delivery (at most 4 batches per
  `MAX_PROBE_BATCHES_PER_DELIVERY = 4` and 30 s of wall time per
  `PROBE_DELIVERY_BUDGET_MS = 30_000`, both in `src/queue_consumer.rs`),
  because a cap-sized NZB (~120 batches = `MAX_SEGMENTS = 60_000` in
  `src/nzb_intake.rs` ÷ `MAX_PROBE_SEGMENTS = 500` in `src/probe.rs`,
  × 500 sequential `STAT`s) would otherwise run into the 15-minute
  consumer invocation limit (pinned by the `const` asserts in
  `src/queue_consumer.rs`) before ever persisting, and every redelivery
  would restart at batch 0. A budget stop persists the partial tally and
  *continues*: a fresh copy of the message is enqueued (5 s delay per
  `PROBE_CONTINUATION_DELAY_SECS = 5` in `src/queue_consumer.rs`,
  matching the first shared-backoff step `retry_delay_secs(1) = 5` in
  `src/retry.rs` via `payload_retry_delay_ms(0)` in `src/dispatch.rs`)
  and the original acked, spending neither the checkpoint attempt budget
  nor a native `max_retries` slot (`Disposition::Continue` in
  `src/retry.rs`; native slots are `CONSUMER_MAX_RETRIES` in
  `src/retry.rs`); a failed continuation enqueue falls back to a native
  retry (`Settlement::Continue` fallback in `src/queue_consumer.rs`). A
  busy/unavailable pool (`PoolProbeError::Retry` in `src/pool_probe.rs`)
  redelivers no sooner than its `Retry-After` and the shared backoff
  (`decide_checkpoint` in `src/retry.rs` honors the hint only as a floor
  clamped to `MAX_HINTED_RETRY_DELAY_SECS`; pool defaults
  `DEFAULT_RETRY_SECS`/`MAX_RETRY_SECS` in `src/pool_probe.rs`), inside
  the same attempt budget; a refused envelope (`Rejected` in
  `src/pool_probe.rs`) fails the job with the generic `probe_failed`
  reason (`ERR_PROBE_FAILED` in `src/retry.rs`). Unit tests cover the
  fold/resume/retry/budget policy against a scripted pool fake; no live
  Worker runs in CI.

The match/write steps are wired in the consumer (`src/matching.rs` +
`src/queue_consumer.rs`): MB release search through the `MB_API` service
binding, the idempotent library upsert through `LIBRARY_DB`, and one
meter event per transition, with the terminal `Done` checkpoint written
only after the upsert commits. After a release pick the matcher fetches
the release's recordings to fill `recording_mbids` in album order
(medium, then track, from the mirror's `mb_track` membership); a
recordings-fetch failure never loses the release match (transient
retries the job, permanent writes the release with an empty list).
Still blocked for the live
half: wiring the consumer's search step to a Newznab provider call
(search-sourced jobs therefore cannot reach the probe yet), provider
settings in the pool DO, verify/repair to the Container runtime, and
the library schema itself — no migration creates the upsert's `library`
table or its `UNIQUE(idempotency_key)` (`migrations/0003_library.sql`
defines only `genres`/`playlists`), and per ADR 0002 the follow-up adds
normalized tables and changes the post-match writer after inspecting
staging rather than assuming the flat table exists. Enabling the
consumer before that lands would fail every `Writing` delivery
(deterministic SQLSTATEs such as 42P01/42P10 are mapped to a permanent
failure so they quarantine at once). Until those land the consumer
stays disabled and no live provider, pool, repair, library, or metering
invocation is claimed here.

## Slice T2 — retry budget + DLQ (this slice)

- Retry budget (`src/retry.rs`): 1 delivery + 4 redeliveries
  (`jobs::MAX_ATTEMPTS` = 5 = `CONSUMER_MAX_RETRIES` + 1 =
  `PAYLOAD_RETRY_ATTEMPTS` + 1, enforced by a compile-time assert).
  The durable checkpoint in `INGEST_STATE` is authoritative because Cloudflare
  redelivers the unchanged Queue body. `decide_checkpoint` increments the
  checkpoint before returning Retry; the handler must persist it before calling
  `retry_with_options`. Backoff is derived from the payload-fetch schedule.
  One settlement is exempt from the budget: a progress continuation
  (`JobError::progress` → `Disposition::Continue`, today only the probe
  budget stop above) persists new checkpoint progress, enqueues a fresh
  message copy, and acks — no attempt increment, no native retry slot —
  because each continuation must carry new progress toward a finite total.
  The consumer settles **per message**, so one poison job costs retries but
  does not head-of-line block valid jobs. Permanent failures skip the budget
  and quarantine at once; quarantine marks the checkpoint Failed before ack. If a DLQ send fails, the consumer must call `retry_with_options` with a delay
  (or return an error) for that message; merely skipping `ack()` and returning
  implicitly acknowledges it. Each retry also spends one of the four native
  `max_retries` slots, after which Cloudflare moves the raw body to its native
  DLQ if `dead_letter_queue` is configured; otherwise Cloudflare drops it. A
  successful send followed by an ack failure may create a duplicate DLQ
  entry, so DLQ processing must be idempotent by `job_id`.
- DLQ quarantine: on exhaustion the consumer sends one `DlqEntry` to
  `nntune-ingest-dlq` (`INGEST_DLQ` binding) — the original `QueueMsg` plus
  machine-readable `error_code` / `attempts` fields in the serialized DLQ
  body — then acks the original. The native dead-letter route can be enabled
  as a safety net for messages the handler never settles (panic, timeout),
  which arrive as raw bodies with no reason envelope.
- Poison coverage (`tests/retry_dlq.rs`): an always-failing job burns all 5
  deliveries, lands in the DLQ with reason body fields asserted, and never
  blocks the valid jobs behind it in the same batch.

## Redrive (DLQ back to queue)

1. On **Workers Paid**, the Cloudflare DLQ defaults to **4 days retention**;
   configure up to 14 days with
   `npx wrangler queues update nntune-ingest-dlq --message-retention-period-secs 1209600`
   (60–1,209,600 seconds). On **Workers Free**, retention is fixed at 24 hours
   and cannot be configured. Unconsumed entries are deleted at expiry on either
   plan, so treat the applicable retention period as the redrive deadline; this
   queue is not a permanent archive.
2. Read the DLQ: each entry is either a `DlqEntry` (explicit quarantine, with
   `error_code` / `attempts` / redacted, bounded `reason`) or a raw `QueueMsg`
   (native dead-letter fallback). Fix the cause first.
3. Load the matching `INGEST_STATE` checkpoint. The redrive helpers reject a
   checkpoint that is not `Failed`, so for a raw native-DLQ body first mark any
   nonterminal checkpoint `Failed` with code `native_dead_letter` and persist it.
   A `Done` checkpoint must not be redriven. Then call
   `retry::redrive(&entry, &mut checkpoint)` (or
   `retry::redrive_raw(&msg, &mut checkpoint)`). The helper resets the durable
   checkpoint and returns a matching message; persist the reset under the same
   job id before enqueue. Identity and input are kept, budgets/state are reset,
   and `probe_tally` / `failure_code` / `failure_reason` are cleared. Never
   enqueue if the checkpoint write failed.
4. No paging on DLQ depth in this slice (out of scope): inspect queue depth
   before bulk redrive so a systemic cause does not re-poison the queue faster
   than it drains.

## Develop

```sh
cargo test -p nntune-ingest
```

Pure modules carry inline unit tests; handlers stay thin so the contract
is reviewed, not mocked.
