# Repair container

Batch-only par2 repair image for the ingest pipeline (ADR 0001 §4,
ADR 0009). The image pins `par2cmdline-turbo` (built from the release
tarball, sha256-verified in the Dockerfile) and a small native Rust
HTTP server (`nntune-repair-server`).

The container holds **no** vault, NNTP or R2 credentials and needs no
outbound network: the `nntune-repair` Worker (`workers/repair`) is the
only caller. It streams staged scratch objects in, asks for one bounded
repair, and streams the outputs back.

## Internal HTTP protocol

Only reachable through the hosting Durable Object's container port.

| Route | Purpose |
|---|---|
| `GET /health` | readiness probe |
| `PUT /jobs/{job_id}/r{round}/in/{key}` | stage one input object (`key` = sha256 of the file name) |
| `POST /repair` | run one repair for a staged manifest; idempotent per `(job_id, round)` |
| `GET /jobs/{job_id}/r{round}/out/{key}` | stream one repaired output back |
| `POST /cleanup` | delete one round's staged and output bytes |

`POST /repair` runs `repair_with_limits` from `nntune-core`: hard
wall-clock timeout, byte caps, and kernel CPU/memory/file-size limits
via `prlimit`. Terminal results (`repaired`, `unrepairable`) are
stored and replayed for a repeated request; timeouts and resource
failures return stable retryable codes and persist nothing.

## Build

From the repo root:

```sh
docker build -f containers/repair/Dockerfile -t nntune-repair:dev .
```

Native tests (fake `par2` fixtures, real `prlimit` wrapping):

```sh
cargo test -p nntune-repair-server
```
