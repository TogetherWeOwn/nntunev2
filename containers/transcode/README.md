# Transcode container

Batch-only `opus-64` transcode image for the ingest pipeline (spec §4).
The image pins the Debian `ffmpeg` (libopus-enabled) from a
digest-pinned slim base plus a small native Rust HTTP server
(`nntune-transcode-server`). The encode contract itself — pinned
ffmpeg argv plus the Ogg serial normaliser — lives in
`nntune-core::transcode` and is shared by every producer, so bytes
and hashes coincide whichever path produced them. (Sources: `crates/nntune-core/src/transcode.rs:70-93` (pinned argv); `:214-226` (serial normaliser); `containers/transcode/Dockerfile:5-14` (base-digest pin rationale); `:16-25` (server package build, `--locked`); `:27-37` (runtime has only `ffmpeg`/`ffprobe` + server, `USER transcode`); `containers/transcode/Cargo.toml:2` (package name).)

The container holds **no** vault, NNTP or R2 credentials and needs no
outbound network: the `nntune-transcode` Worker (`workers/transcode`)
is the only caller. It stages one verified audio object in, asks for
one bounded encode, and streams the artifact back. (Sources: `containers/transcode/src/lib.rs:3-9` (no creds, no outbound, caller + bounded encode).)

## Internal HTTP protocol

Only reachable through the hosting Durable Object's container port. (Sources: `containers/transcode/src/lib.rs:48` (`SERVER_PORT = 8080`).)

| Route | Purpose |
|---|---|
| `GET /health` | readiness probe |
| `PUT /jobs/{job_id}/t/{staging_id}/in/{key}` | stage the verified input object (`key` = `{index:04}-{sha256(file_name)}`) |
| `POST /transcode` | run one encode for the staged input; idempotent per `(job_id, staging_id)` |
| `GET /jobs/{job_id}/t/{staging_id}/out/artifact.ogg` | stream the normalised artifact back |
| `POST /cleanup` | delete one staging's bytes |

(Sources: `containers/transcode/src/lib.rs:661-663` (`GET /health`); `:697-718` (`PUT .../in/{key}`); `:664-671` (`POST /transcode`); `:719-739` (`GET .../out/artifact.ogg`); `:672-696` (`POST /cleanup`); `:639-656` (path shape); `crates/nntune-core/src/transcode.rs:276-286` (input-key rule).)

`POST /transcode` re-checks the staged bytes against the request's
`source_sha256`, runs the pinned argv with an in-process wall-clock
budget (300 s default, 600 s max), rejects empty, oversize (> 32 MiB)
or non-Ogg encoder output, and normalises the Ogg serial to 1 before
hashing and storing the artifact. Terminal results (`transcoded`,
`untranscodable`) are stored and replayed for a repeated request for
the same staging; the same input under a new `staging_id` is a new
directory and reruns; a different input for one staging conflicts
(409). Timeouts and signal kills return stable retryable codes and
persist nothing. No `prlimit` CPU/memory wrapper: inputs are
verified-intact audio under the 64 MiB staging ceiling, not hostile
damage, so the wall timeout plus the instance memory bound the cost.
(Sources: `containers/transcode/src/lib.rs:215-270` (idempotency + input re-check); `:272-296` (bounded encode, timeout/signal mapping); `:298-343` (output validation, normalisation, stored verdicts); `:189-209` (wall-clock runner); `:56-71` (budgets and ceilings); `:19-30` (no-`prlimit` rationale).)

## Single-object input boundary

- The transcode input is one staged whole object: the ingest
  `Transcoding` step only records verified-intact, single-object audio
  files (empty `spool`), and the Container refuses a missing, empty,
  oversize or hash-mismatched staged input without running the
  encoder. (Sources: `containers/transcode/src/lib.rs:160-179` (request validation); `:260-270` (staged-input checks).)
- A verify staging for a file too large to assemble in memory instead
  records the file as its ordered spool-segment key list, which this
  input cannot consume; such files are excluded from the recorded
  transcode inputs with a terminal reason naming the boundary.
  Assembling spool segments into one encode input is future work.

## Build and test

From the repo root (build context is the repo root):

```sh
docker build -f containers/transcode/Dockerfile -t nntune-transcode:dev .
```

Native tests (stub encoder scripts, real HTTP + timeouts):

```sh
cargo test -p nntune-transcode-server
```

Image self-test (proves the shipped encoder: libopus capability,
two-staging byte-identical determinism, ffprobe-valid `opus-64`):

```sh
bash containers/transcode/tests/determinism_self_test.sh nntune-transcode:dev
```

Runtime-user pin (the runtime stage must end non-root owning
`/scratch`; run in CI with `--self-test`):

```sh
bash containers/transcode/tests/test_runtime_user.sh
```

(Sources: `containers/transcode/Dockerfile:3` (repo-root context); `containers/transcode/tests/determinism_self_test.sh:8-15` (assertions); `containers/transcode/tests/test_runtime_user.sh:2-13` (pin statement).)
