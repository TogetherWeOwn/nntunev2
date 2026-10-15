# workers/ingest — ingest pipeline (slices 1–2: contracts + Queue wiring)

User-scoped ingest jobs via Queues/Workflows:

```text
Indexer search (BYO key) ─┐
                          ├─→ parse (nntune-core) → pool-DO probe → par2 verify ─┬─→ repair (Container batch)
NZB upload ───────────────┘                                                      └─→ tag/fingerprint + MB match (workers/mb) → library rows
```

## This slice

- Job state machine (`src/jobs.rs`): `Queued → Searching|Parsing →
  Probing → Verifying → (Repairing → Verifying)? → Matching → Writing →
  Done`, `Failed` terminal. Checkpoints make redelivery/replay converge;
  idempotency keys bind user + content sha + source ref (`:`-free fields, so
  triples cannot collide). Repair rounds are budgeted (`MAX_REPAIR_ROUNDS`):
  a perpetually failing verify ends in `Failed`, never in a loop.
- Newznab search builder (`src/newznab.rs`): `t=music`, `https://` only,
  no embedded credentials. The URL is built **without** the key; the
  vault-resolved key is injected at request time and redacted in logs.
- NZB intake guard (`src/nzb_intake.rs`): 2 MiB / 5k files / 60k segments
  caps before anything spends provider connections.
- Per-plan rate limits (`src/rate_limit.rs`): provisional base/5x/20x
  tunables (exact numbers are set at launch); token buckets, no clock
  dependency.
- Queue/Workflow payloads (`src/queue.rs`): `QueueMsg`, `RepairRequest`,
  `LibraryWrite` — all carry the idempotency key. MB calls target the
  `workers/mb` read API (`/v1/mb/release/:mbid`, `/v1/mb/recording/:mbid`,
  `/v1/mb/search?type=…`).
- Routes (`src/lib.rs`): `POST /v1/ingest/search` (16 KiB body cap,
  ≤16 categories, paged window), `POST /v1/ingest/nzb` (wire cap accounting
  for JSON escaping, then the 2 MiB / 5k / 60k intake guard) — malformed JSON
  is 400, over-cap is 413, busy store/queue is 503; success is 202 + enqueue
  with job id + idempotency key. `GET /v1/ingest/jobs/:id` (checkpoint reads
  land with the consumer).

## Security rules

- `indexer_ref` is an opaque vault handle. Raw indexer/provider keys are
  never accepted in API bodies, never logged (redacted URLs only), never
  committed. `http://` indexer URLs are refused.
- Upload bodies are untrusted: capped, shape-checked, never logged.

## E2E contract (synthetic NNTP server card)

End-to-end acceptance runs against the synthetic server in CI (no real
Usenet in CI). Ingest needs from that card:

1. Fixture articles: yEnc single/multi-part, obfuscated subjects, one
   intentionally gapped set (430s) + matching par2 set for the repair path.
2. Newznab stub: `t=music` search returning a GUID whose NZB references
   fixture message-ids.
3. Connection caps per credential + slow-link/mid-disconnect modes for the
   resume test (kill mid-`Probing`, redeliver, expect one library row).

## Slice 2 (this slice)

- Queue wiring (`src/dispatch.rs`): routes build a `QueueMsg` carrying the
  full job input (search params, or a content-addressed `INGEST_PAYLOADS`
  handle for upload bytes written before enqueue) with a user-bound
  idempotency key, and `send` it to `nntune-ingest-jobs` (`worker` `queue`
  feature; bindings in `wrangler.toml.example`). Search keys bind query +
  indexer + categories + limit + page, so no request is silently dropped.
  Success is 202 `queued`; a missing/failing binding or payload store is 503
  with `Retry-After` (never 202). The consumer/workflow entries stay
  commented out in `wrangler.toml.example` until the Queue handler (with DLQ)
  and the Workflow class ship — declaring them now would drop every message.
- Request bodies carry self-asserted `user_id` (1..=128 chars) until the
  accounts card authenticates callers; the key format already binds the
  user, so keys stay valid when auth lands.
- Hyperdrive library upsert (`src/library.rs`): one row per idempotency
  key, `ON CONFLICT DO UPDATE` so replays converge on the latest MB match.
- Repair dispatch (`src/repair.rs`): missing blocks + par2 set → Container
  `POST /repair`; missing blocks with no par2 set fails the job.
- Metering (`src/metering.rs`): one `nntune_ingest_meter` event per
  workflow step (workers-rs 0.3 has no Analytics Engine client, so the edge
  write goes through the dataset binding's HTTP API).
- Probe envelope + resume mapping (`src/probe.rs`), parse shape +
  intake cross-check + probe batching (`src/parse.rs`), synthetic fixtures
  (`src/fixtures.rs`), pipeline E2E (`tests/pipeline_e2e.rs`): happy path
  reaches `Done` with one library row; the gapped set repairs then finishes.

Blocked on siblings for the live half: pool-DO wire contract, `nntune-core`
parse output, synthetic NNTP fixture server + CI E2E against it, checkpoint
KV reads in `GET /v1/ingest/jobs/:id`, and the repair Container runtime.

## Slice T2 — retry budget + DLQ (this slice)

- Retry budget (`src/retry.rs`): 1 delivery + 4 redeliveries
  (`jobs::MAX_ATTEMPTS` = 5 = `CONSUMER_MAX_RETRIES` + 1 =
  `PAYLOAD_RETRY_ATTEMPTS` + 1, enforced by a compile-time assert).
  Redeliveries back off 5/15/30/60 s (`retry_delay_secs`), spaced across the
  ~60 s KV read lag instead of hot-looping. The consumer settles **per
  message** (`decide` / `drive_batch`: ack on success, delayed retry while
  `jobs::can_retry` allows it), so one poison job ahead of valid ones costs
  retries, never head-of-line blocking. Permanent failures (unparseable
  bytes) skip the budget and quarantine at once.
- DLQ quarantine: on exhaustion the consumer sends one `DlqEntry` to
  `nntune-ingest-dlq` (`INGEST_DLQ` binding) — the original `QueueMsg` plus
  machine reason headers `x-ingest-error-code` / `x-ingest-attempts` — then
  acks the original. The native dead-letter route stays configured as the
  safety net for messages the handler never settles (panic, timeout), which
  arrive as raw bodies with no reason envelope.
- Poison coverage (`tests/retry_dlq.rs`): an always-failing job burns all 5
  deliveries, lands in the DLQ with reason headers asserted, and never
  blocks the valid jobs behind it in the same batch.

## Redrive (DLQ back to queue)

1. Read the DLQ (`nntune-ingest-dlq`): each entry is either a `DlqEntry`
   (explicit quarantine, with `error_code` / `attempts` / `reason`) or a raw
   `QueueMsg` (native dead-letter fallback).
2. Fix the cause first (bad bytes, downed dependency) — redrive resets both
   the attempt and the repair-round budgets, so a still-poison message just
   burns the fresh budget and quarantines again.
3. Re-enqueue with `retry::redrive(&entry)` (or `retry::redrive_raw(&msg)`
   for native bodies): same job id, idempotency key and input, `attempt`
   reset to 1, back at `Queued`. A redriven job that now succeeds acks like
   any first delivery.
4. No paging on DLQ depth in this slice (out of scope): poll the queue depth
   before redriving in bulk so a systemic cause does not re-poison the queue
   faster than it drains.

## Develop

```sh
cargo test -p nntune-ingest
```

Pure modules carry inline unit tests; handlers stay thin so the contract
is reviewed, not mocked.
