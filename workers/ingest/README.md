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
- Outbound indexer fetch SSRF guard (`src/indexer_fetch.rs`, same ranges
  as the `workers/api` vault guard — keep the two lists in sync): https
  only, no embedded credentials, 443-only port allowlist (any other
  explicit port is refused before any dial, so the fetch can never be
  aimed at another port as a scan primitive). Every fetch resolves ALL
  DNS records and rejects loopback / link-local (incl. the cloud metadata
  endpoint) / private / unique-local / CGNAT / multicast / broadcast /
  documentation / reserved space (v4+v6, incl. IPv6 transition space with
  a public-looking payload); one private record poisons the whole answer.
  DNS-pin invariant: each hop's host is resolved exactly once and the
  transport dials only that vetted address set on 443, retaining the
  hostname for TLS SNI / certificate checks and the Host header — the
  transport never re-resolves, so a changed second DNS answer (rebinding)
  is never consulted. Every redirect hop (301/302/303/307/308, absolute /
  protocol-relative / path-absolute) is re-validated from scratch and at
  most 3 hops are followed; the `apikey=` value is never forwarded to a
  redirect target. Dial timeout 5000 ms per hop, total fetch timeout
  15000 ms (the vault outbound numbers, reused); at the deadline counts
  as expired. Guard blocks answer a stable 4xx, timeouts 504, upstream
  failures 502, with stable codes for the shared taxonomy envelope:
  `indexer_fetch_bad_scheme`, `indexer_fetch_malformed_url`,
  `indexer_fetch_embedded_credentials`, `indexer_fetch_port_not_allowed`,
  `indexer_fetch_forbidden_address`, `indexer_fetch_unresolvable_host`,
  `indexer_fetch_too_many_redirects`, `indexer_fetch_dial_timeout`,
  `indexer_fetch_timeout`, `indexer_fetch_upstream_status`. Decision logs
  record host + code only (`url_host_for_log`); full URLs go through
  `newznab::redact_api_key` before any other log line.

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

## Develop

```sh
cargo test -p nntune-ingest
```

Pure modules carry inline unit tests; handlers stay thin so the contract
is reviewed, not mocked.
