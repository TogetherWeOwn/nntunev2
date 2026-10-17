# mb-import — dump import + hourly replication (batch only)

Container image for the two batch jobs behind `nntunev2-mb`
(ADR 0007). Interactive traffic never runs here.

## Jobs

| Job | Trigger | Script | SLO |
|---|---|---|---|
| Initial import | manual Container run | `import.sh <curated-dir>` (curated `mbdump-<table>.tsv`; missing input is fatal; optional `DUMP_SEQ=<n>` seeds `mb_import_state.last_packet` forward-only) | reruns are idempotent and move rows and the replication cursor forward only (`WHERE updated_at < EXCLUDED.updated_at`; `DUMP_SEQ` seeds `last_packet` only when it advances the cursor, never touches `last_applied_at`/`lag_seconds`, so an older dump never rolls back newer rows or rewinds replication); seed `last_packet` via `DUMP_SEQ` (or manually) before replication or `replicate.sh` keeps refusing packets |
| Replication | hourly schedule (Container / Workflow cron) | `replicate.sh` with `PACKET_SEQ` + `PACKET_URL` | **lag < 2 h** via `mb_import_state.lag_seconds`; state is written only when a packet is actually applied, packets must arrive in sequence, and the pending apply step exits non-zero without touching state |

## Initial dump ETL

Run `DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>` before
`import.sh <mbdump-dir>/curated`. The input directory must contain the official,
uncompressed, headerless MusicBrainz COPY files (for example `artist`,
`release`, and `recording`) plus the lookup/relationship files listed in
`etl.py`. The ETL writes the three curated `mbdump-<table>.tsv` files into an
immutable snapshot and atomically swaps `<mbdump-dir>/curated` to point to it;
previous snapshots are retained. It uses the image's Python 3 and a temporary,
disk-backed SQLite projection (no external database). Run the fixture check
with `bash containers/mb-import/tests/test_etl.sh` (from the repository root);
the Dockerfile's test stage runs it during the normal image build without
shipping fixtures in the runtime image.

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
- `caa_url` is emitted as
  `https://coverartarchive.org/release/<release-mbid>/front-250`; it is a
  deterministic Cover Art Archive endpoint, not proof artwork exists.
- A recording's release is obtained through `track.medium -> medium.release`.
  If a recording appears more than once, select by lowest release table id,
  then medium position, track position, and track id. `track_no` is the selected
  track's position in its medium; duration comes from nullable
  `recording.length` milliseconds.
- Every row gets the single supplied `DUMP_TS`; no row-specific time is created.
  Missing inputs, wrong field counts, and non-empty entity input that projects
  to zero rows fail closed.

## Secrets

`DATABASE_URL` comes from the secret store (GitHub Actions secrets /
Cloudflare / `~/secure-drop`) — never committed, never logged. Scripts
read it from env only.

## Rollback

Import and replication are additive upserts; rollback is "stop the job,
keep serving the previous packet" (`mb_import_state.last_packet` shows
the last good one). No destructive migrations in this path.
