# MB mirror synthetic-fixture seed

Seeds the MusicBrainz mirror with the one release the staging E2E ingests
(`e2e/fixtures/synthetic.nzb`, 660 Hz sine track). The seed is
[`synthetic-fixture.sql`](synthetic-fixture.sql). Full fail-closed
verification (empty/drift/repair matrix) lives in the paired
acceptance-test change, not in this PR; the row-display queries below run
at this head with no extra files.

Seeded rows: `mb_artist` `11111111-…`, `mb_release` `22222222-…`
(`NNTune-Fixture sine-660`, exactly the NZB-derived match query),
`mb_recording` `33333333-…` (same title, track 1, 210 ms), and the one
`mb_track` membership (medium 1, track 1). Every statement is an idempotent
upsert: re-running inserts nothing new and repairs drift back to the fixture.

## Local run (scratch database only)

Requires `psql` on `PATH` and a scratch Postgres 18 database. Never staging
or production.

```sh
export DATABASE_URL=postgres://$USER@localhost:5432/mb_seedcheck
createdb --host=localhost --port=5432 --username="$USER" mb_seedcheck
for m in 0001_mb_mirror 0014_mb_import_resume_dlq 0016_mb_track \
         0017_mb_replication_packet_ledger; do
  psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -f migrations/${m}.sql
done
# Apply the seed (re-runnable upsert).
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/seed/synthetic-fixture.sql
# Show the four fixture rows (one row each).
psql "$DATABASE_URL" -X -c "SELECT mbid, name FROM mb_artist WHERE mbid = '11111111-1111-1111-1111-111111111111'::uuid"
psql "$DATABASE_URL" -X -c "SELECT mbid, title FROM mb_release WHERE mbid = '22222222-2222-2222-2222-222222222222'::uuid"
psql "$DATABASE_URL" -X -c "SELECT mbid, title, track_no, duration_ms FROM mb_recording WHERE mbid = '33333333-3333-3333-3333-333333333333'::uuid"
psql "$DATABASE_URL" -X -c "SELECT release_mbid, recording_mbid, medium_no, track_no FROM mb_track WHERE release_mbid = '22222222-2222-2222-2222-222222222222'::uuid"
# Re-run the seed: still one row per table (idempotent upsert).
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/seed/synthetic-fixture.sql
```

The connection travels in the `DATABASE_URL` environment variable only.
No credentials live in this directory or in the SQL file.

## Staging apply (documented here, executed elsewhere)

Applying this seed to the staging mirror is **out of scope** for this
change. The target is the single shared `nntunev2-mb` mirror: the same
database prod reads share (the mb worker's `MB_DB` Hyperdrive is one
config across envs; see the worker bindings inventory in `docs/ops.md`).
The synthetic rows become visible to prod `/v1/mb/search` reads. Apply
only with explicit mirror-owner approval, executed by an authorized host
executor with the importer role; the executor re-runs the paired
acceptance-test verification (separate change) read-only as evidence. Do
not paste credentials, URLs, hostnames, or login identities into issues,
comments, or the repo.
