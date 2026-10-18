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

## Staged-input shape boundary

- The repair input accepts one staged whole object per manifest file row, except `data` rows with `size` 0, which name missing files the Container recreates from recovery blocks and have no staged object (`workers/repair/src/runtime.rs`, `containers/repair/src/lib.rs`): each row carries `file_name`, `key`, `size`, and `role` (`workers/repair/src/contract.rs`), and `key` must equal `sha256(file_name)` (the filename-derived scratch object key, not a hash of the staged bytes) (`containers/repair/src/lib.rs`, `crates/nntune-core/src/par2/scratch.rs`).
- For each staged (non-missing) row, the repair worker streams exactly that keyed object from scratch into the Container and rejects a size mismatch (`workers/repair/src/runtime.rs`); the Container stages one input object per `PUT /jobs/{job_id}/r{round}/in/{key}` (`containers/repair/README.md`, `containers/repair/src/lib.rs`). Size-0 `data` rows are skipped before staging (`workers/repair/src/runtime.rs`).
- A verify staging for a file too large to assemble in memory instead records the file as its ordered spool-segment key list (`workers/ingest/src/verify.rs`): `StagedFile::spool` lists the segment keys in order (`workers/ingest/src/verify.rs`), and the manifest row carries them in `segments` in file order with gone articles indexed in `missing` (`workers/ingest/src/verify.rs`).
- Spool-segment consumption is not yet available (`workers/ingest/src/verify.rs`, `workers/repair/src/contract.rs`): the repair-side manifest shape carries no `segments`/`spool` field and its input contract reads whole objects only (`workers/repair/src/contract.rs`, `workers/ingest/src/verify.rs`), so a staged manifest whose rows carry non-empty `segments` is not repairable through this input today (`workers/ingest/src/verify.rs`, `workers/repair/src/contract.rs`). Concatenating spool segments into a single bytestream (zero-filling gaps named in `missing`) is future work (`workers/ingest/src/verify.rs`).

## Build

From the repo root:

```sh
docker build -f containers/repair/Dockerfile -t nntune-repair:dev .
```

Native tests (fake `par2` fixtures, real `prlimit` wrapping):

```sh
cargo test -p nntune-repair-server
```
