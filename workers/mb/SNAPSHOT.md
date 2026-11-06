# R2 snapshot backend — phase 0 spike

Measurement-only. Every deployed `workers/mb` environment leaves
`MB_BACKEND` unset and keeps the Hyperdrive repository; nothing here
changes the `/v1/mb/*` contract or any production configuration.

The question: can an immutable snapshot of the curated CC0 subset in R2,
read through `SnapshotRepo` (`src/snapshot.rs`) behind the existing
`MbRepo` trait, replace the PlanetScale `nntunev2-mb` cluster?

## Pieces

| Piece | Where |
|---|---|
| Streaming extract (CC0 `mbdump.tar.bz2` only; allowlisted tables, gzip-1) | `containers/mb-import/stream_extract.py` |
| Curated projection (unchanged rules; `.gz` inputs/outputs behind env flags) | `containers/mb-import/etl.py` |
| Snapshot format, search scoring, repository, spike router | `src/snapshot.rs` |
| Snapshot builder / local read path | `examples/snapshot_build.rs`, `examples/snapshot_eval.rs` |
| Golden set, `pg_trgm` baseline, recall, latency bench | `spike/` |
| Orchestration | `.github/workflows/mb-snapshot-spike.yml` |

## Format

Per kind (`artist`, `release`, `recording`, `release-search`) under
`snap/<dump>/`: a 1 MiB `<kind>.idx` (65,536 × 16-byte entries) and
`<kind>.dat.<part>` objects (≤ 256 MiB) of raw-DEFLATE blocks holding
sorted `key\tvalue` lines. Lookups bucket by the MBID's first four hex
digits (one range read once the index is cached in the isolate). Release
search reads the exact-title key plus up to four word keys in one parallel
round, then scores candidates with a port of `pg_trgm` `similarity()` over
diacritic-folded text (`src/snapshot_fold.tsv`), keeps scores ≥ 0.3 and
orders by score, title and MBID like the Postgres `search_sql`.

Spike scope: lookups and release search. `release_recordings`, the W6b
listings and artist/recording search return the generic 500 on this
backend.

## Gates (Operator-set) and how each is measured

| Gate | Pass | Method |
|---|---|---|
| 1 Recall | snapshot top-5 ≥ `pg_trgm` top-5 on the golden set | `spike/golden.py` → `spike/pg_baseline.py` (verbatim release `search_sql`, GIN `gin_trgm_ops`) vs `snapshot_eval search` → `spike/recall.py` |
| 2 Latency | uncached lookup p95 < 250 ms; uncached search p95 < 600 ms | `spike/bench.py`: 1,000 requests at 50 concurrent per endpoint, distinct keys, no KV/edge cache; client time plus `Server-Timing` and R2 read count |
| 3 ETL | ≤ 2 h, within the 20 GB disk cap | `spike/measure.sh`: whole pipeline in `docker run --cpus 4 --memory 12g` on an exactly 20 GB filesystem |

Disclosed deviations: gate 3 runs on a GitHub runner held to the
Container standard-4 envelope, not in a Cloudflare Container (not enabled
in any environment yet); CPU speed differs. No ingest query log exists
yet, so the golden set is built from dump-sampled releases through the
ingest matcher's query path (`spike/golden.py`).

## Running

- PR label `mb-snapshot-spike`: build + recall, no secrets.
- `workflow_dispatch` on `main` with `stage: true`: also uploads to
  `nntunev2-mb-snap-staging` (14-day expiry rule on `snap/`), deploys the
  token-gated `nntune-mb-snap-staging` Worker, runs the bench, then
  deletes the Worker and the objects.

Results are the run's step summary and the `mb-snapshot-spike-results`
artifact.
