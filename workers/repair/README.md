# Repair worker (`nntune-repair`)

The no-route Worker that fronts the par2 repair Container
(`containers/repair`) for the ingest pipeline.

- **No public surface.** `workers_dev = false`, no routes: the only
  caller is the ingest Worker's `REPAIR_CONTAINER` service binding
  (`POST /repair`). Every other method/path is a 404.
- **One Durable Object per job.** Requests are validated, then routed
  to `RepairContainer` (`id_from_name(job_id)`), so a job's repair
  rounds serialise behind one object and one Container.
- **Idempotent rounds.** If `out/result.json` already exists for
  `(job_id, round)` and carries the current manifest's digest, the
  stored verdict is replied without a second paid Container run. A
  redrive resets the round to 1 with freshly staged inputs, so the
  digest check is what keeps a previous generation's result from
  replaying; on mismatch the Container's stale round is cleaned and
  the repair reruns. Otherwise staged inputs are streamed from the
  `INGEST_SCRATCH` R2 bucket into the Container, one bounded repair
  runs, outputs stream back to `out/`, and only then is
  `out/result.json` written. Ingest receives a small status reply
  (`{status, exit_code, file_count}`); the full per-file result
  document stays in `out/result.json`.
- **Container isolation.** The Container is started with internet
  access disabled and holds no vault, NNTP or R2 credentials; it sees
  only staged bytes and the manifest. Errors are stable
  machine-readable codes and logs carry job ids only.

Scratch layout and validation rules are the shared contract in
`nntune_core::par2::scratch`; request/manifest/result shapes live in
`src/contract.rs` (native-tested).

## Deploy gate

Not deployed yet. Deploying requires Cloudflare Containers enabled on
the existing plan with no new spend; that confirmation and the deploy
itself are operator actions. Until then `wrangler.toml` is the deploy
contract and ingest stays unable to finish a `Repairing` job (its
consumer also remains disabled).

## Checks

```sh
cargo test --manifest-path workers/repair/Cargo.toml --all-targets
cargo clippy --manifest-path workers/repair/Cargo.toml --all-targets -- -D warnings
cargo check --manifest-path workers/repair/Cargo.toml --target wasm32-unknown-unknown
```
