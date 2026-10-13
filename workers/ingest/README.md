# workers/ingest — ingest pipeline (slice 1: contracts + edge guards)

User-scoped ingest jobs via Queues/Workflows (bindings land in slice 2):

```text
Indexer search (BYO key) ─┐
                          ├─→ parse (nntune-core) → pool-DO probe → par2 verify ─┬─→ repair (Container batch)
NZB upload ───────────────┘                                                      └─→ tag/fingerprint + MB match (workers/mb) → library rows
```

## This slice

- Job state machine (`src/jobs.rs`): `Queued → Searching|Parsing →
  Probing → Verifying → (Repairing → Verifying)? → Matching → Writing →
  Done`, `Failed` terminal. Checkpoints make redelivery/replay converge;
  idempotency keys bind user + content sha + source ref.
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
- Routes (`src/lib.rs`): `POST /v1/ingest/search`, `POST /v1/ingest/nzb`
  (202 + validated envelope), `GET /v1/ingest/jobs/:id`. Queue/Workflow/DB
  wiring returns `not_wired` until slice 2.

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

## Slice 2 (not here)

Queue/Workflow bindings, Hyperdrive library writes, pool-DO probe calls,
Container repair dispatch, `nntune-core` parse integration, Analytics
Engine metering events.

## Develop

```sh
cargo test -p nntune-ingest
```

Pure modules carry inline unit tests; handlers stay thin so the contract
is reviewed, not mocked.
