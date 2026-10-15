# mb-import — dump import + hourly replication (batch only)

Container image for the two batch jobs behind `nntunev2-mb`
(ADR 0007). Interactive traffic never runs here.

## Jobs

| Job | Trigger | Script | SLO |
|---|---|---|---|
| Initial import | manual Container run | `import.sh <dump-dir>` (curated `mbdump-<table>.tsv`; missing input is fatal; optional `DUMP_SEQ=<n>` seeds `mb_import_state.last_packet` forward-only; `MB_IMPORT_CHUNK_LINES=<n>` sizes chunks, default 2000) | kill-safe and idempotent: each file applies in one-transaction chunks (validate via `mb-validate` -> `COPY` -> typed upsert -> cursor advance), the committed line number lands in `mb_import_cursor`, and poison rows land in `mb_import_dlq` with reason + raw line instead of aborting the run. Reruns skip completed chunks and insert zero dupes; the upsert moves rows forward only (`WHERE updated_at < EXCLUDED.updated_at`; `DUMP_SEQ` seeds `last_packet` only when it advances the cursor, never touches `last_applied_at`/`lag_seconds`, so an older dump never rolls back newer rows or rewinds replication); seed `last_packet` via `DUMP_SEQ` (or manually) before replication or `replicate.sh` keeps refusing packets. A repeated MBID inside/across chunks or a cursor ahead of the dump is fatal (fix the dump; the cursor never auto-rewinds) |
| Replication | hourly schedule (Container / Workflow cron) | `replicate.sh` with `PACKET_SEQ` + `PACKET_URL` | **lag < 2 h** via `mb_import_state.lag_seconds`; state is written only when a packet is actually applied, packets must arrive in sequence, and the pending apply step exits non-zero without touching state |

## Operator queries

```sql
-- What still needs loading (per table).
SELECT table_name, lines_done FROM mb_import_cursor ORDER BY 1;
-- Poison rows, newest first; raw_line keeps the exact dump bytes.
SELECT table_name, line_no, reason, raw_line FROM mb_import_dlq
  ORDER BY created_at DESC LIMIT 20;
```

## Build and test

The image builds from the repository root (the validator stage needs the
workspace): `docker build -f containers/mb-import/Dockerfile .`.
`test-import.sh` runs the integration suite (chunked import, kill-resume,
dupe-rerun, DLQ, refusal guards) against a scratch database:

```sh
cargo build -p mb-import-validate
DATABASE_URL=<scratch-url> ALLOW_TEST_TRUNCATE=1 \
  MB_VALIDATE_BIN=$PWD/target/debug/mb-validate \
  bash containers/mb-import/test-import.sh
```

The suite truncates the mirror tables, so `ALLOW_TEST_TRUNCATE=1` is
mandatory and the target must be scratch — never staging or production.
Unit tests for the validator run with the workspace: `cargo test`.

## Secrets

`DATABASE_URL` comes from the secret store (GitHub Actions secrets /
Cloudflare / `~/secure-drop`) — never committed, never logged. Scripts
read it from env only.

## Rollback

Import and replication are additive upserts; rollback is "stop the job,
keep serving the previous packet" (`mb_import_state.last_packet` shows
the last good one). No destructive migrations in this path.
