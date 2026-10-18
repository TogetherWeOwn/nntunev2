# Repair container

Batch-only par2 repair image for the ingest pipeline (ADR 0001 §4,
ADR 0009). The image pins `par2cmdline-turbo` (built from the release
tarball, sha256-verified in the Dockerfile) and a small native Rust
HTTP server (`nntune-repair-server`). (Sources: `docs/adr/0001-nntune-core-transport-and-simd.md:28-30` (repair native-only); `docs/adr/0009-stack.md:32,71` (containers batch-only); `containers/repair/Dockerfile:7-20` (1.5.0 tarball + sha256 check); `containers/repair/Dockerfile:22-29`, `containers/repair/Cargo.toml:2` (server package build).)

The container holds **no** vault, NNTP or R2 credentials and needs no
outbound network: the `nntune-repair` Worker (`workers/repair`) is the
only caller. It streams staged scratch objects in, asks for one bounded
repair, and streams the outputs back. (Sources: `containers/repair/src/lib.rs:1-16` (no creds, no outbound, caller + bounded repair); `workers/repair/src/runtime.rs:282-298` (`enable_internet(false)`, health-gated `get_tcp_port(8080)`); `containers/repair/Dockerfile:31-43` (runtime has only `par2` + server, `USER repair`).)

## Internal HTTP protocol

Only reachable through the hosting Durable Object's container port. (Sources: `workers/repair/src/runtime.rs:296-298`; `containers/repair/src/lib.rs:34-36` (`SERVER_PORT = 8080`).)

| Route | Purpose |
|---|---|
| `GET /health` | readiness probe |
| `PUT /jobs/{job_id}/r{round}/in/{key}` | stage one input object (`key` = sha256 of the file name) |
| `POST /repair` | run one repair for a staged manifest; idempotent per `(job_id, round, staging_id)` |
| `GET /jobs/{job_id}/r{round}/out/{key}` | stream one repaired output back |
| `POST /cleanup` | delete one round's staged and output bytes |

(Sources: `containers/repair/src/lib.rs:641-643` (`GET /health`); `:673-687` (`PUT .../in/{key}`); `:644-651` (`POST /repair`); `:688-706` (`GET .../out/{key}`); `:652-672` (`POST /cleanup`).)

`POST /repair` runs `repair_with_limits` from `nntune-core`: hard
wall-clock timeout, byte caps, and kernel CPU/memory/file-size limits
via `prlimit`. Terminal results (`repaired`, `unrepairable`) are
stored and replayed for a repeated request for the same staging
(`job_id`, round, `staging_id`); a new `staging_id` over the same file
set reruns, a different file set conflicts. Timeouts and resource
failures return stable retryable codes and persist nothing. (Sources: `containers/repair/src/lib.rs:255-272` (limits + timeout/resource mapping); `crates/nntune-core/src/par2/repair.rs:14-36` (budgets), `:87-108` (Linux `prlimit` required, fail-closed); `crates/nntune-core/src/par2/repair/linux.rs:165-170` (`/usr/bin/prlimit --cpu/--fsize/--as`); `containers/repair/Dockerfile:32-33` (`util-linux` provides `prlimit`); `containers/repair/src/lib.rs:183-223` (stored `request.json`/`result.json` replay, `staging_id` generation check, `409 repair_manifest_conflict`); `:274-323` (terminal vs `503 repair_timeout` / `repair_resource` / `repair_failed`); `:107-125`, `workers/repair/src/contract.rs:324-349` (stable `{"error": code}` taxonomy).)

## Staged-input shape boundary

- The repair input accepts one staged whole object per manifest file row, except `data` rows with `size` 0, which name missing files the Container recreates from recovery blocks and have no staged object: each row carries `file_name`, `key`, `size`, and `role`, and `key` must equal `sha256(file_name)` (the filename-derived scratch object key, not a hash of the staged bytes). (Sources: `containers/repair/src/lib.rs:51-68` (`ScratchFile` shape + roles); `:155-161` (`key == object_key_for_file_name`); `:236-249` (size-0 `data` recreated, no staged input); `workers/repair/src/contract.rs:87-100` (mirror shape); `crates/nntune-core/src/par2/scratch.rs:86-90` (`sha256(file_name)` key derivation).)
- For each staged (non-missing) row, the repair worker streams exactly that keyed object from scratch into the Container and rejects a size mismatch; the Container stages one input object per `PUT /jobs/{job_id}/r{round}/in/{key}`. Size-0 `data` rows are skipped before staging. (Sources: `workers/repair/src/runtime.rs:209-216` (skip size-0); `:311-359` (`stage_input`, `object.size() != file.size` → `scratch_input_mismatch`); `containers/repair/src/lib.rs:341-396` (`store_input`, aggregate cap).)
- A verify staging for a file too large to assemble in memory instead records the file as its ordered spool-segment key list: `StagedFile::spool` lists the segment keys in order, and the manifest row carries them in `segments` in file order with gone articles indexed in `missing`. (Sources: `workers/ingest/src/verify.rs:149-181` (`StagedFile` + `spool`); `:427-444` (`ManifestEntry.segments`/`missing`).)
- Spool-segment consumption is not yet available: the repair-side manifest shape carries no `segments`/`spool` field and its input contract reads whole objects only, so a staged manifest whose rows carry non-empty `segments` is not repairable through this input today. Concatenating spool segments into a single bytestream (zero-filling gaps named in `missing`) is future work. (Sources: `workers/ingest/src/verify.rs:427-444`; `workers/repair/src/contract.rs:87-100` (no `segments` field); `workers/repair/src/runtime.rs:209-216,311-359` (whole-object staging only).)

## Build

From the repo root (build context is the repo root):

```sh
docker build -f containers/repair/Dockerfile -t nntune-repair:dev .
```

Native tests (fake `par2` fixtures, real `prlimit` wrapping):

```sh
cargo test -p nntune-repair-server
```

(Sources: `containers/repair/Dockerfile:3` (repo-root context); `containers/repair/tests/repair_server.rs:1-5` (fake `par2`, real `prlimit` invocation); `containers/repair/Cargo.toml:2` (package name).)
