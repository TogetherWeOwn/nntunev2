# ADR 0001: Serverless Cloudflare Stack (Decided)

- **Status:** Accepted
- **Date:** 2026-10-13
- **Non-blocking note:** Sibling cards already build on these assumptions; this ADR
  confirms them. It does not gate any in-flight work.

## Context

NNTune v2 is a BYO-Usenet-provider music streaming service. Users bring their
own Usenet provider account; we provide search, streaming, library management
and offline-friendly caching. The v2 rewrite must be serverless-first on
Cloudflare, keep all relational data in one Postgres, and share media
codecs between edge and native targets.

## Decision

| Concern | Choice |
|---|---|
| Backend | **Rust** (`workers-rs` → `wasm32`) for **ALL** backend Workers, including Durable Objects |
| Shared core | `crates/nntune-core`: NNTP client, SIMD yEnc, NZB parsing, decrypt, par2-verify — compiled to `wasm32` **and** native |
| Frontend | **SvelteKit** on Cloudflare (`adapter-cloudflare`) |
| Relational data | **PlanetScale Postgres via Cloudflare Hyperdrive** for ALL relational data (no D1): app database `nntunev2` + MusicBrainz mirror database `nntunev2-mb` in one account, each via its own Hyperdrive binding |
| Object cache | **R2** per-env buckets `nntunev2-{audio,art,archive}-{staging,prod}` (key layout in ADR 0003) |
| Ingest | **Queues + Workflows** |
| Config | **KV** |
| Metering | **Analytics Engine** |
| Batch only | **Containers** — MusicBrainz import, par2 repair (everything interactive stays serverless) |

### Repo layout (agreed)

```text
crates/nntune-core   # shared Rust core (wasm32 + native)
workers/{api,stream,ingest,mb}  # Rust workers-rs backends incl. Durable Objects
apps/web             # SvelteKit frontend (adapter-cloudflare)
migrations/{app,mb}/  # Postgres migrations: app DB vs mirror DB, independent sequences
docs/adr/            # this directory
```

> `apps/web` is canonical. The scaffold README's `web/` spelling is superseded;
> the scaffold card owns that rename (non-blocking here).

### Alternatives considered

- **TypeScript Workers backend:** rejected — yEnc/par2 hot paths need SIMD Rust,
  and one language for edge + native avoids a dual implementation.
- **D1 for relational data:** rejected — the data model (ADR 0002) needs Postgres
  features (partitioning, full-text, pgvector-adjacent MB matching); PlanetScale
  via Hyperdrive keeps one relational platform (app DB + mirror DB,
  ADR 0002 rule 8).
- **Next.js / self-hosted frontend:** rejected — SvelteKit + adapter-cloudflare
  keeps the whole product on the Cloudflare edge with zero servers.

## Consequences

- All backend PRs are Rust (`cargo fmt --check`, `cargo clippy`, `cargo test`).
- `nntune-core` must stay `#![no_std]`-compatible where feasible so `wasm32`
  builds never regress; CI builds both targets.
- Durable Objects are written in Rust via `workers-rs`; no TS backend code.
- Containers are batch-only; any PR proposing an interactive Container needs a
  new ADR.
