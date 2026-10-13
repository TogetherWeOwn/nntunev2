# NNTune v2

Serverless BYO-Usenet-provider music streaming on Cloudflare.

## Vision: BYOP

Users **bring their own Usenet provider** — NNTune v2 never bundles or
resells Usenet access. You connect your existing provider account; we provide
the music experience on top: search, streaming, library management and
offline-friendly caching.

## Plans

Usage-based plans, in the spirit of Claude Pro/Max style tiers (e.g. a base
plan plus 5x / 20x usage multipliers). Exact limits are set at launch; the
metering primitives below are built to support them from day one.

## Architecture

| Concern | Choice |
|---|---|
| Backend | **Rust** (`workers-rs` → `wasm32`) for **ALL** backend Workers, including Durable Objects |
| Shared core | `crates/nntune-core`: NNTP client, SIMD yEnc, NZB parsing, decrypt, par2-verify — compiled to `wasm32` **and** native |
| Frontend | **SvelteKit** on Cloudflare (`adapter-cloudflare`) |
| Relational data | **PlanetScale Postgres via Cloudflare Hyperdrive** for ALL relational data, including our own MusicBrainz mirror |
| Object cache | **R2** for audio / artwork cache |
| Ingest | **Queues + Workflows** |
| Config | **KV** |
| Metering | **Analytics Engine** |
| Batch only | **Containers** — MusicBrainz import, par2 repair (everything interactive stays serverless) |

```text
crates/nntune-core   # NNTP client, SIMD yEnc, NZB, decrypt, par2-verify (wasm32 + native)
workers/             # Rust workers-rs backends incl. Durable Objects
web/                 # SvelteKit frontend (adapter-cloudflare)
```

## Secrets (provisioned by the DevOps card — never committed)

| Secret | Purpose |
|---|---|
| `CLOUDFLARE_API_TOKEN` | Cloudflare deploys (Workers, R2, KV, Queues) |
| `CLOUDFLARE_ACCOUNT_ID` | Cloudflare account scope |
| `HYPERDRIVE_*` ids | PlanetScale Postgres connection(s) via Hyperdrive |

No secret values live in this repo. CI fails closed on leaks (`gitleaks`
required check; secret scanning + push protection enabled).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Every PR must pass `ci-ok`,
`gitleaks`, `pr-lint` and Paperclip Review 5/5.

## Security

See [SECURITY.md](SECURITY.md). Please report vulnerabilities privately via
GitHub's private vulnerability reporting.

<!-- ci-probe: throwaway PR to verify hosted runners -->
