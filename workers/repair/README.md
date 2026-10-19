# Repair worker (`nntune-repair`)

The no-route Worker that fronts the par2 repair Container
(`containers/repair`) for the ingest pipeline.

- **No public surface.** `workers_dev = false`, no routes: the only
  caller is the ingest Worker's `REPAIR_CONTAINER` service binding
  (`POST /repair`). Every other method/path is a 404.
  (Sources: `wrangler.toml:18` and the `NOT YET DEPLOYED` header;
  `src/runtime.rs:74-77,132-135`; `workers/ingest/wrangler.toml.example:129-133`;
  `workers/ingest/src/repair.rs:18-24`.)
- **One Durable Object per job.** Requests are validated, then routed
  to `RepairContainer` (`id_from_name(job_id)`), so a job's repair
  rounds serialise behind one object and one Container.
  (Sources: `src/runtime.rs:32,86-89,144-148`;
  `wrangler.toml:33-39`.)
- **Idempotent rounds.** If `out/result.json` already exists for
  `(job_id, round)` and carries the current staging's digest, the
  stored verdict is replied without a second paid Container run. The
  digest covers the manifest's per-staging `staging_id` — verify
  derives it from the staged files' content hashes, so identical
  bytes keep the id and any byte change yields a new one — because
  a redrive resets the round to 1 and re-stages the same file names
  at the same sizes with different bytes; names and sizes alone
  would replay the previous generation's verdict. On mismatch the
  Container's stale round is cleaned, the stale `out/result.json`
  is deleted, and only then does the repair stage and rerun, so a
  retry inside the new generation replays the Container's own
  result instead of paying twice. Otherwise staged inputs are
  streamed from the `INGEST_SCRATCH` R2 bucket into the Container,
  one bounded repair runs, outputs stream back to `out/`, and only
  then is `out/result.json` written. Ingest receives a small status
  reply (`{status, exit_code, file_count}`); the full per-file
  result document stays in `out/result.json`.
  (Sources: `src/contract.rs:210-212,234-251,412-419`
  (`manifest_digest`, `plan`, `ingest_reply_body`);
  `src/runtime.rs:343-432` (`stage_input`, `/repair` call,
  result-marker-last); `workers/ingest/src/verify.rs:270-277,2071-2083`
  — `staging_id` derived from content hashes.)
- **Container isolation.** The Container is started with internet
  access disabled and holds no vault, NNTP or R2 credentials; it sees
  only staged bytes and the manifest. Errors are stable
  machine-readable codes and logs carry job ids, rounds, statuses
  and counts, never file names.
  (Sources: `src/runtime.rs:282-294`;
  `src/contract.rs:288-311,322-347`;
  `containers/repair/src/lib.rs:1-16`.)

Scratch layout and validation rules are the shared contract in
`nntune_core::par2::scratch`; request/manifest/result shapes live in
`src/contract.rs` (native-tested).

## Deploy gate

Not deployed yet (verified 2026-10-17). Deploying requires Cloudflare
Containers enabled on the existing plan with no new spend; that
confirmation and the deploy itself are operator actions. Until then
`wrangler.toml` is the deploy contract and ingest stays unable to
finish a `Repairing` job (its consumer also remains disabled).
(Sources: `wrangler.toml:11-13`;
`workers/ingest/wrangler.toml.example:30-31,52-56`;
`workers/ingest/src/repair.rs:146-187` (`call_repair`) — binding,
transport and body-read failure is `Retry`, never a terminal outcome.)

## Checks

```sh
cargo test --manifest-path workers/repair/Cargo.toml --all-targets
cargo clippy --manifest-path workers/repair/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/repair/Cargo.toml -- --check
cargo check --manifest-path workers/repair/Cargo.toml --target wasm32-unknown-unknown
cargo clippy --manifest-path workers/repair/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
```

(Mirrors CI job `rust (workers/repair)`;
Sources: `.github/workflows/ci.yml:430-451`.)
