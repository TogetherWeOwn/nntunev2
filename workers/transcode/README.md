# Transcode worker (`nntune-transcode`)

The no-route Worker that fronts the `opus-64` transcode Container
(`containers/transcode`) for the ingest pipeline.

- **No public surface.** `workers_dev = false`, no routes: the only
  caller is the ingest Worker's `TRANSCODE_CONTAINER` service binding
  (`POST /transcode`). Every other method/path is a 404.
  (Sources: `wrangler.toml:15,18` and the `NOT YET DEPLOYED` header
  `:11-13`; `src/runtime.rs:129-147`;
  `workers/ingest/wrangler.toml.example:163-168` (service binding);
  `workers/ingest/src/transcode.rs:95-100` (binding contract),
  `:323-324` (`call_transcode`).)
- **One Durable Object per job.** Requests are validated, then routed
  to `TranscodeContainer` (`id_from_name(job_id)`), so a job's encodes
  serialise behind one object and one Container. The object refuses
  envelopes for any other job.
  (Sources: `src/runtime.rs:34,140-147,197,207-218`;
  `wrangler.toml:33-40`.)
- **Idempotent stagings.** If `out/result.json` already exists for the
  job and carries the current request's `staging_id`, the stored
  verdict is replied without a second paid Container run. The
  staging id is content-derived (ingest derives it from the input's
  content hash), so the previous input's verdict — same job, other
  staging — never replays for this input. On mismatch the
  Container's staging state is cleaned, the stale `out/result.json`
  is deleted, and only then does the input stage and rerun, so a
  retry inside the new generation replays the Container's own
  result instead of paying twice. Otherwise the staged input is
  streamed from the `INGEST_SCRATCH` R2 bucket into the Container,
  one bounded encode runs, the artifact is fetched back, verified
  against the reply's hash and size, and persisted to `out/` — and
  only then is `out/result.json` written. Ingest receives a small
  status reply (`{status, artifact_sha256, byte_size}`); the bytes
  stay in scratch for ingest to fetch.
  (Sources: `src/contract.rs:165-197` (`plan`),
  `:283-290` (`ingest_reply_body`);
  `src/runtime.rs:243-250` (marker read + plan), `:251-276`
  (stale-generation cleanup), `:278-308` (stage, `/transcode`
  call, validate, verified fetch, marker-last write).)
- **Verified artifacts.** A `Transcoded` Container result is
  shape-checked before any byte is fetched, and the fetched bytes
  must match the claimed size and SHA-256 before the R2 put — a
  corrupt or substituted artifact fails here, never as a published
  cache row. `Untranscodable` skips the fetch entirely.
  (Sources: `src/contract.rs:205-220`
  (`validate_transcoded_result`), `:231-245` (`verify_artifact`);
  `src/runtime.rs:288-295`.)
- **Container isolation.** The Container is started with internet
  access disabled and holds no vault, NNTP or R2 credentials; it sees
  only staged bytes and input identity (no user ids, idempotency keys
  or credentials cross). Errors are stable machine-readable codes
  and logs carry job ids, statuses and byte counts, never file names.
  (Sources: `src/runtime.rs:334-356`, `:561-573`;
  `src/contract.rs:319-326`;
  `containers/transcode/src/lib.rs:3-9`.)

Scratch layout and validation rules are the shared contract in
`nntune_core::transcode` and `nntune_core::par2::scratch`;
request/result shapes live in `src/contract.rs` (native-tested).

## Deploy gate

Not deployed yet. Deploying requires Cloudflare Containers enabled on
the existing plan with no new spend; that confirmation and the deploy
itself are operator actions. Until then `wrangler.toml` is the deploy
contract and ingest stays unable to finish a `Transcoding` job (its
consumer also remains disabled).
(Sources: `wrangler.toml:11-13`;
`workers/ingest/src/transcode.rs:298-329` (`call_transcode`) —
binding, transport and body-read failure is `Retry`, never a terminal
outcome.)

## Outer-hop deadlines

The front-to-object call has a **600 s** total cutoff
(`DO_TRANSCODE_DEADLINE_MS` in `src/runtime.rs`); ingest's service call has
**660 s** (`TRANSCODE_CALL_DEADLINE_MS` in
`workers/ingest/src/transcode.rs`). Each timer is polled before dispatch
and covers headers **and** the capped body until EOF. Partial
headers/chunks do not replenish it. The settlement owns the cancellable
`worker::Delay`, releasing it on success, transport or cap refusal, and
expiry. Ingest tests can inject milliseconds through
`call_transcode_with_deadline`; both native settlement seams accept a
controlled deadline future.

These are finite cutoff policies, **not** worst-case capacity promises:

- The child's **330 s** `/transcode` exchange covers the synchronous
  ffmpeg run (default native wall limit 300 s) and its result read,
  **not** the rest of the object round. The front allows another 270 s
  shared by result reads, health, input staging, artifact relay,
  marker write and cleanup.
- Transfer budgets are individually size-scaled (`10_000 + size / 1024`
  milliseconds). The 64 MiB input ceiling permits a single staging
  allowance of about 75 s; the 32 MiB artifact ceiling permits about
  43 s for fetch plus put. Health has 60 attempts, each with up to
  a 10 s exchange and a 500 ms sleep (about 630 s total), not a hard
  30 s wall-clock ceiling. The outer cutoff can therefore expire while
  an encode is still within its individual resource limits.
- The enclosing queue invocation contract is **900 s**. One 660 s wait
  leaves 240 s for other work; this does **not** bound a serial batch of
  messages. The example consumer's batch size of 10 must be reconciled
  with that invocation contract before activation. This change leaves the
  consumer disabled and changes no batching, retries or rollout gates.

Expiry preserves the existing transient verdicts: `TranscodeCall::Retry`
at ingest and 503 `transcode_unavailable` at the front. Completed replies
retain existing status classification and byte caps (64 KiB at ingest;
1 MiB results / 32 MiB artifacts at the front), without returning
truncated prefixes. Synthetic tests drive pending headers,
stalled/dribbling bodies, cap boundaries and later retries through the
same settlement seams as the runtime adapters.

**Local settlement is not remote cancellation evidence.** Dropping a Rust
future and its timer does not prove that the remote Worker, object or
Container stopped. An encode may still complete and write its marker after
the caller times out. Existing staging-id idempotency and
result-marker-last ordering are unchanged; a later legitimate retry can
replay a completed matching staging without manufacturing terminal success.
No real service bindings or Containers are exercised by these tests.

## Checks

```sh
cargo test --manifest-path workers/transcode/Cargo.toml --all-targets
cargo clippy --manifest-path workers/transcode/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path workers/transcode/Cargo.toml -- --check
cargo check --manifest-path workers/transcode/Cargo.toml --target wasm32-unknown-unknown
cargo clippy --manifest-path workers/transcode/Cargo.toml --target wasm32-unknown-unknown -- -D warnings
```

(Mirrors CI job `rust (workers/transcode)`;
Sources: `.github/workflows/ci.yml:594-615`.)
