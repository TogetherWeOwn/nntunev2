# ADR 0009: Serverless Cloudflare Stack (Decided)

- **Status:** Accepted design; implementation and independent review gates remain.
- **Date:** 2026-10-13
- **Revised:** 2026-10-15 — unique number, authoritative metering and current
  migration layout. The decided stack is unchanged.
- **Renumbering:** this was `0001-stack.md` on the architecture branch. Main's
  [ADR 0001](0001-nntune-core-transport-and-simd.md) owns the core transport/SIMD
  number; stack citations to the former draft now mean **ADR 0009**.

## Context

NNTune v2 is a BYO-Usenet-provider music streaming service. Users bring their
own Usenet provider account; we provide search, streaming, library management
and offline-friendly caching. The rewrite is serverless-first on Cloudflare,
uses one relational platform (separate app and mirror databases), and shares
media codecs between edge and native targets.

## Decision

| Concern | Choice |
|---|---|
| Backend | **Rust** (`workers-rs` → `wasm32`) for **all** backend Workers, including Durable Objects |
| Shared core | `crates/nntune-core`: NNTP client, SIMD yEnc, NZB parsing, decrypt, par2 verification — compiled to `wasm32` **and** native; transport details in ADR 0001 |
| Frontend | **SvelteKit** on Cloudflare (`adapter-cloudflare`) |
| Relational data | **PlanetScale Postgres via Cloudflare Hyperdrive**, no D1. App cluster `nntunev2`, staging database `nntunev2_staging`; separate shared `nntunev2-mb` mirror cluster. Binding names and provisioned targets come from `docs/ops.md`; a production binding is not approval to write or migrate its target |
| Object cache | **R2**, environment-qualified `nntunev2-{audio,art,archive}-{staging,prod}` (ADR 0003) |
| Ingest | **Queues + Workflows** |
| Config | **KV**, not authoritative entitlements or usage counters |
| Usage enforcement | **Postgres** daily flow ledger and live cache-grant gauge; pool Durable Object leases for concurrency (ADR 0004) |
| Observability | **Analytics Engine**, sampled telemetry only — never a billing/cap authority |
| Batch only | **Containers** — MusicBrainz import, par2 repair; interactive paths stay serverless |

### Repo layout

```text
crates/nntune-core             # shared Rust core (wasm32 + native)
workers/{api,stream,ingest,mb}  # Rust workers-rs backend layout, incl. Durable Objects
apps/web                      # canonical SvelteKit frontend, scaffold already merged
migrations/                   # top-level SQL; explicit per-file app/mirror target
docs/adr/                     # this directory
```

This is the agreed component layout, not a claim every component is implemented.
Main already contains `migrations/0001_mb_mirror.sql` and
`migrations/0002_auth_vault.sql`, with no SQL migration runner. Keep their applied
paths/history; do not invent an existing `migrations/{app,mb}/` split or apply both
files to both databases. ADR 0002 rule 7 defines the additive runner/manifest
requirement. Production app DB provisioning/migration remains gated separately;
the current binding alone does not establish a clean production app database.

### Alternatives considered

- **TypeScript backend Workers:** rejected for this architecture. Rust shares
  the codec implementation between native batch and edge targets.
- **D1 for relational data:** rejected. The data model uses Postgres constraints,
  partitions and relational queries; Hyperdrive keeps one relational platform
  across app and mirror databases (ADR 0002).
- **Next.js / self-hosted frontend:** rejected. SvelteKit + adapter-cloudflare
  preserves the decided Cloudflare deployment model.

## Consequences

- Backend changes pass Rust format/lint/tests and native/wasm target checks.
  Portability follows the core transport/features contract in ADR 0001; this
  stack ADR does not silently impose a different core runtime or feature set.
- Durable Objects use Rust via `workers-rs`; no parallel TypeScript backend.
- Containers are batch-only; an interactive Container requires a new ADR.
- These ADRs confirm or adjust implementation contracts without restarting
  parallel work. Already-merged code needs additive alignment where identified;
  existing CI, review, security and release gates still apply.
