# MB mirror synthetic-fixture seed

Seeds the MusicBrainz mirror with the one release the staging E2E ingests
(`e2e/fixtures/synthetic.nzb`, 660 Hz sine track). The seed is
[`synthetic-fixture.sql`](synthetic-fixture.sql); the paired verification is
`workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql` plus
`e2e/tests/mb-mirror-seed.spec.ts`, which this change never edits.

Seeded rows: `mb_artist` `11111111-…`, `mb_release` `22222222-…`
(`NNTune-Fixture sine-660`, exactly the NZB-derived match query),
`mb_recording` `33333333-…` (same title, track 1, 210 ms), and the one
`mb_track` membership (medium 1, track 1). Every statement is an idempotent
upsert: re-running inserts nothing new and repairs drift back to the fixture.

## Local run (scratch database only)

Requires `psql` on `PATH` and a scratch Postgres 18 database. Never staging
or production.

```sh
createdb mb_seedcheck
export DATABASE_URL=postgres://$USER@localhost/mb_seedcheck
for m in 0001_mb_mirror 0014_mb_import_resume_dlq 0016_mb_track \
         0017_mb_replication_packet_ledger; do
  psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 -f migrations/${m}.sql
done
# Empty DB: the verification MUST fail (missing rows fail by name).
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql
# Apply the seed, re-verify: MUST pass.
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/seed/synthetic-fixture.sql
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql
# Re-run the seed, re-verify: still green (idempotent upsert).
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/seed/synthetic-fixture.sql
psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 \
  -f workers/mb/tests/sql/mirror_seed_synthetic_fixture_verify.sql
```

The connection travels in the `DATABASE_URL` environment variable only.
No credentials live in this directory or in the SQL file.

## Staging apply (documented here, executed elsewhere)

Applying this seed to the staging mirror is **out of scope** for this
change. An authorized host executor applies `synthetic-fixture.sql` to the
staging MB mirror cluster (the shared read-oriented `nntunev2-mb` mirror;
staging reads share it) with the importer role, then re-runs the
verification SQL read-only as evidence. Do not paste credentials, URLs,
hostnames, or login identities into issues, comments, or the repo. The
un-fixme of the live `recording_mbids` leg in
`e2e/tests/mb-mirror-seed.spec.ts` is a future slice once that apply lands.
