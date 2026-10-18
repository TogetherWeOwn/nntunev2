# mb-import — dump import + hourly replication (batch only)

Container image for the two batch jobs behind `nntunev2-mb`
(ADR 0007). Interactive traffic never runs here.

## Jobs

| Job | Trigger | Script | SLO |
|---|---|---|---|
| Initial import | manual Container run | `import.sh <curated-dir>` (`mbdump-<table>.tsv`; missing input is fatal; optional `DUMP_SEQ=<n>` seeds `mb_import_state.last_packet` forward-only; `MB_IMPORT_CHUNK_LINES=<n>` sizes chunks, default 2000) | kill-safe and idempotent: each file is keyed by its SHA-256 in `mb_import_cursor`, then applied in one-transaction chunks (validate via `mb-validate` -> `COPY` -> typed upsert -> cursor advance). The same dump resumes after its last committed chunk; a changed dump gets a fresh cursor even if it has the same or fewer lines. Poison rows land in `mb_import_dlq` per dump with reason + raw line instead of aborting the run. Duplicate valid keys within one dump — MBIDs on the entity tables, the `(release, medium, track)` position on `mbdump-track.tsv` — or a cursor ahead of that dump are fatal; the same MBID in a later dump is allowed and uses the forward-only upsert (`WHERE updated_at < EXCLUDED.updated_at`). `DUMP_SEQ` seeds `last_packet` only when it advances the cursor, never touches `last_applied_at`/`lag_seconds`, so an older dump never rolls back newer rows or rewinds replication. Seed `last_packet` via `DUMP_SEQ` (or manually) before replication or `replicate.sh` keeps refusing packets. (Sources: `import.sh:76-80` (`MB_IMPORT_CHUNK_LINES`, default 2000); `:121-126` (missing input fatal); `:182-210` (SHA-256 `dump_id`, whole-file dupe / cursor-ahead refusal); `:270-279` (per-chunk dupe check); `:387-404` (one-transaction chunk); `:416-442` (`DUMP_SEQ` forward-only seed); `Dockerfile:40-41` (scheduler overrides default CMD).) |
| Replication | hourly schedule (Container / Workflow cron) | `replicate.sh` with `PACKET_SEQ` + `PACKET_URL` | **lag < 2 h** SLO (each packet applied within 2 h of publication); per-packet evidence is `mb_replication_packet.lag_secs`, not the aggregate snapshot — `mb_import_state.lag_seconds` is written only by the pending apply step. Packets must arrive in sequence (`PACKET_SEQ = last_packet + 1`), and until the apply step lands the script exits non-zero without touching state. (Sources: `docs/adr/0007-mb-mirror.md:24-26` (hourly schedule, SLO); `replicate.sh:2-9` (SLO + pending apply), `:16-19,36-44` (integer + sequence gate), `:46-51` (exit 2, no state write); `migrations/0017_mb_replication_packet_ledger.sql`; `workers/mb/README.md:74-81` (SLO evidence); `migrations/0001_mb_mirror.sql:57-65` (state columns).) |

## Initial dump ETL

Run `DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>` before
`import.sh <mbdump-dir>/curated`. The input directory must contain the official,
uncompressed, headerless MusicBrainz COPY files (for example `artist`,
`release`, and `recording`) plus the lookup/relationship files listed in
`etl.py`. The ETL writes the four curated `mbdump-<table>.tsv` files into an
immutable snapshot and atomically swaps `<mbdump-dir>/curated` to point to it;
previous snapshots are retained. It uses the image's Python 3 and a temporary,
disk-backed SQLite projection (no external database). Run the fixture checks
with `bash containers/mb-import/tests/test_etl.sh` and
`bash containers/mb-import/tests/test_etl_reference_graphs.sh` (from the
repository root — the latter pins four broken-reference refusal diagnostics:
missing artist_type, release credit-name rows, track recording, medium
release);
the Dockerfile's test stage runs them during the normal image build without
shipping fixtures in the runtime image. (Sources: `etl.sh:1-14`;
`etl.py:141-160` (`DUMP_TS`, temp SQLite projection), `:319-335` (immutable
snapshot + atomic `curated` swap, previous snapshots retained);
`tests/test_etl.sh`; `Dockerfile:9-17,27-38`;
`.github/workflows/ci.yml:298-313`.)

The ETL's positional mapping follows the official
[MusicBrainz `CreateTables.sql`](https://github.com/metabrainz/musicbrainz-server/blob/master/admin/sql/CreateTables.sql)
field order; use an extract from the matching schema revision. Derivations:

- Artist `type` resolves the nullable `artist.type` through `artist_type.name`;
  `country` resolves `artist.area` to its `iso_3166_1.code` (otherwise NULL).
- Release `artist_mbid` uses the first `artist_credit_name` by ascending position
  for the release's artist credit. `release_type` resolves
  `release_group.type` through `release_group_primary_type.name` (otherwise
  NULL). `year` is the minimum non-NULL raw `date_year` across release events
  in `release_country` and `release_unknown_country` (otherwise NULL).
  (Sources: `etl.py:20-50` (pinned positions/field counts), `:234-253`
  (artist/release queries).)
- `caa_url` is emitted as
  `https://coverartarchive.org/release/<release-mbid>/front-250`; it is a
  deterministic Cover Art Archive endpoint, not proof artwork exists.
- A recording's release is obtained through `track.medium -> medium.release`.
  If a recording appears more than once, select by lowest release table id,
  then medium position, track position, and track id. `track_no` is the selected
  track's position in its medium; duration comes from nullable
  `recording.length` milliseconds. That single canonical release is only
  the recording row's own pointer: `mbdump-track.tsv` additionally emits
  every track appearance as `(release_mbid, recording_mbid, medium_no,
  track_no)` — medium position, then track position within the medium —
  so a release listing keeps shared recordings and true album order
  (`mb_track`, migration 0016). FK orphans in that file land in the DLQ as
  `fk_missing_release` / `fk_missing_recording`; negative positions are
  validator poison (`negative_position`). (Sources: `etl.py:261-304`
  (canonical recording, membership rows); `migrations/0016_mb_track.sql`;
  `import.sh:292-297` (track FK specs);
  `crates/mb-import-validate/src/lib.rs:263-267` (`Position` check).)
- Every row gets the single supplied `DUMP_TS`; no row-specific time is created.
  Missing inputs, wrong field counts, and non-empty entity input that projects
  to zero rows fail closed. (Sources: `etl.py:73-87` (missing input, field
  counts), `:121-129` (`DUMP_TS`), `:131-137` (single stamp),
  `:198-199,306-315` (fail-closed projections).)
- Snapshots curated before `mb_track` have no `mbdump-track.tsv`: re-run
  the ETL for such a dump instead of pointing the new `import.sh` at it
  (the import refuses the missing file after the earlier tables load;
  cursors make the rerun resume, but the curated set is the unit).
  (Sources: `import.sh:121-126` (tables load in order, missing file fatal).)

## Operator queries

```sql
-- What still needs loading (per table).
SELECT table_name, dump_id, lines_done FROM mb_import_cursor ORDER BY 1, 2;
-- Poison rows, newest first; raw_line keeps the exact dump bytes.
SELECT table_name, dump_id, line_no, reason, raw_line FROM mb_import_dlq
  ORDER BY created_at DESC LIMIT 20;
```

Reasons are machine-readable: the validator files shape problems as
`column_count`, `empty_line`, `bad_uuid`, `bad_int`, `bad_timestamp`,
`bad_escape` (anything `COPY (FORMAT text)` would load mangled, e.g. `\\o`,
`\\u`, a bare `\\x`), `null_in_not_null`, `bad_country`,
`negative_duration`, `negative_position`, `invalid_encoding`; the loader files FK orphans —
a release/recording/track row whose parent was poisoned or never loaded
(track checks both parents) — as `fk_missing_artist` /
`fk_missing_release` / `fk_missing_recording` instead of aborting the
chunk. (Sources: `crates/mb-import-validate/src/lib.rs:63-76` (reason
codes), `:270-349` (`bad_escape` rule);
`crates/mb-import-validate/src/main.rs:122-135` (`invalid_encoding` on
non-UTF-8 bytes); `import.sh:292-297,313-367`
(FK orphan split); `migrations/0014_mb_import_resume_dlq.sql`;
`migrations/0001_mb_mirror.sql:57-65`.)

## Build and test

Build from the repository root (the validator stage needs the workspace —
it copies the full workspace and builds the single validator package):

```sh
docker build -f containers/mb-import/Dockerfile .
```

(Sources: `Dockerfile:19-25` (validator stage), `:27-38` (runtime stage).)

`test-import.sh` runs the integration suite (chunked import, kill-resume,
same-dump rerun, newer-dump upsert, DLQ, FK orphans, all-poison and
dupe/cursor-ahead refusals, track repoint/prune, interrupted multi-chunk
resume) against a scratch database: (Sources: `test-import.sh:1-8`
(contract), `:82-136,221-260,264-323,324-377,379-525` (suites 1-12);
`.github/workflows/ci.yml:483-516`.)

```sh
cargo build -p mb-import-validate
DATABASE_URL=<scratch-url> ALLOW_TEST_TRUNCATE=1 \
  MB_VALIDATE_BIN=$PWD/target/debug/mb-validate \
  bash containers/mb-import/test-import.sh
```

The suite truncates the mirror tables, so `ALLOW_TEST_TRUNCATE=1` is
mandatory and the target must be scratch — never staging or production.
(Sources: `test-import.sh:11-15` (refusal guard), `:45-49` (truncate + state
reset).) Unit tests for the validator run with the workspace: `cargo test`.
(Sources: `Cargo.toml:2` (workspace member);
`crates/mb-import-validate/src/lib.rs:503-788` (unit tests);
`.github/workflows/ci.yml:51` (`cargo test --workspace`).)

## Secrets

`DATABASE_URL` comes from the secret store — never committed, never
logged by the scripts under normal (non-xtrace) operation. The job scripts (`import.sh`, `replicate.sh`) read it from env
only: `pg-env.sh` translates it into `PG*` variables and unsets it, so
`psql` is invoked with no connection string in its argv (argv hygiene is
pinned by `tests/test_argv_hygiene.sh`). Do not run the scripts under
`bash -x` / `SHELLOPTS=xtrace`: tracing expands the
`: "${DATABASE_URL:?...}"` presence check and `pg-env.sh`'s `eval`,
printing the secret to stderr. (Sources: `import.sh:4-6,67-73,95-99`;
`replicate.sh:12,21-27`; `pg-env.sh:1-12,287-297`;
`tests/test_argv_hygiene.sh`.)

## Rollback

Entity tables are additive forward-only upserts, so rollback is "stop the
job, keep serving the previous packet" (`mb_import_state.last_packet`
shows the last good one). Exception: `mb_track` prunes positions the
newest dump no longer lists — but only inside the final chunk's
transaction together with the cursor advance, so an interrupted import
leaves rows at worst stale, never truncated. Once the last `mb_track`
chunk commits, the prune is final: an older dump cannot restore it (the
forward-only guard skips its updates and the prune deletes any
older-timestamp rows it inserts), so recovery is a corrected dump stamped
with a newer `DUMP_TS`, or a database restore. A position whose
replacement row landed in the DLQ (poison or FK orphan) also loses its
old row, leaving a visible gap rather than a stale recording. No
data-destroying migrations in this path; 0014/0016 swap constraints in
place (cursor PK, DLQ unique, `table_name` checks). (Sources: `import.sh:152-172`
(prune, DLQ gap), `:381-386` (final-chunk atomicity), `:391-393` (forward-only
guard); `test-import.sh:427-445` (older dump leaves rows alone);
`migrations/0016_mb_track.sql:38-43` (check swaps);
`migrations/0014_mb_import_resume_dlq.sql:28-29,46-49` (PK/unique swaps).)
