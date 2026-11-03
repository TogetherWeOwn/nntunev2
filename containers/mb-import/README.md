# mb-import — dump import (batch only; hourly replication retired)

Container image for the two batch jobs behind `nntunev2-mb`
(ADR 0007). Interactive traffic never runs here.

> **Decision changed (2026-11-03):** [ADR 0012 §5](../../docs/adr/0012-compliant-youtube-video-and-brainz-data.md)
> and the amended [ADR 0007](../../docs/adr/0007-mb-mirror.md) drop the
> hourly Live Data Feed: the mirror is CC0-only on twice-weekly core
> dumps with a ≤4-day freshness target, and no MetaBrainz supporter
> tier, account or token is held. The replication job, its packet
> ledger and the <2 h SLO below describe the superseded hourly design
> that is still in the tree — do not configure schedules or
> monitoring against them for the CC0 mirror. Retiring/replacing
> them is part of the follow-on mirror build change, not done here.

## Jobs

| Job | Trigger | Script | SLO |
|---|---|---|---|
| Initial import | manual Container run | `import.sh <curated-dir>` (ten `mbdump-<table>.tsv` files — `artist`, `release_group`, `release`, `recording`, `track`, `url`, `artist_url`, `recording_url`, `recording_music_video`, `recording_isrc`; missing input is fatal; optional `DUMP_SEQ=<n>` seeds `mb_import_state.last_packet` forward-only; `MB_IMPORT_CHUNK_LINES=<n>` sizes chunks, default 2000) | kill-safe and idempotent: each file is keyed by its SHA-256 in `mb_import_cursor`, then applied in one-transaction chunks (validate via `mb-validate` -> `COPY` -> typed upsert -> cursor advance). The same dump resumes after its last committed chunk; a changed dump gets a fresh cursor even if it has the same or fewer lines. Poison rows land in `mb_import_dlq` per dump with reason + raw line instead of aborting the run. Duplicate valid keys within one dump — MBIDs on the entity and `url` tables, the `(artist, url, link)` / `(recording, url, link)` triples on the URL-relationship tables, the unordered recording pair on `recording_music_video`, the `(recording, isrc)` pair on `recording_isrc`, the `(release, medium, track)` position on `mbdump-track.tsv` — or a cursor ahead of that dump are fatal; the same key in a later dump is allowed and uses the forward-only upsert (`WHERE updated_at < EXCLUDED.updated_at`). Duplicate checks COPY-decode and canonicalize keys the way the validator normalizes them (lowercase UUIDs, plain integers, literal bools, uppercase ISRCs, the music-video pair sorted like the canonical ETL ordering), so an octal/hex-escaped alias for an already-listed key is refused, not loaded beside it. `DUMP_SEQ` seeds `last_packet` only when it advances the cursor, never touches `last_applied_at`/`lag_seconds`, so an older dump never rolls back newer rows or rewinds replication. Seed `last_packet` via `DUMP_SEQ` (or manually) so per-packet lag accounting has a baseline. (Sources: `import.sh:95` (`MB_IMPORT_CHUNK_LINES`, default 2000); `:111-134` (all-file and sequence admission); `:249-250` (SHA-256 `dump_id` + cursor insert); `:251-429` (alias-aware whole-file dupe refusal); `:429-445` (cursor read/ahead/complete); `:501-523` (per-chunk dupe check); `:637-652` (one-transaction chunk); `:674-687` (`DUMP_SEQ` forward-only seed); `Dockerfile:48-49` (scheduler overrides default CMD).) |
| Replication | retired (no schedule) | `replicate.sh` refuses every invocation | Hourly Live Data Feed packets carry the NC licence variant and never enter the CC0-only mirror; product reads come from the twice-weekly CC0 core dumps via the initial-import job above. The retired script exits 2 **before** reading secrets, touching the network, or writing any state, preserving the old pending-apply refusal signal for callers that branch on it. The per-packet SLO contract in ADR 0007 (each applied packet within 2 h of publication, evidence in `mb_replication_packet.lag_secs`) stands for whenever packet application returns under a CC0-compatible feed. (Sources: `replicate.sh:1-11` (retirement, exit 2, no state); `docs/adr/0007-mb-mirror.md:24-26` (SLO); `migrations/0017_mb_replication_packet_ledger.sql`; `migrations/0001_mb_mirror.sql:57-65` (state columns).) |

## Initial dump ETL

Run `DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>` before
`import.sh <mbdump-dir>/curated`. The input directory must contain the official,
uncompressed, headerless MusicBrainz COPY files: the entity files (`artist`
with `artist_type`/`area`/`iso_3166_1` lookups, `release` with
`artist_credit_name`/`release_group`/`release_group_primary_type`/`release_country`/`release_unknown_country`,
`recording` with `medium`/`track`) plus the relationship files (`url`,
`link_type`, `link`, `link_attribute_type`, `link_attribute`,
`l_artist_url`, `l_recording_url`, `l_recording_recording`, `isrc`) listed in
`etl.py`. The ETL writes the ten curated `mbdump-<table>.tsv` files into an
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
`etl.py:197-206` (`DUMP_TS` validation), `:223-228` (temp SQLite projection), `:319-335` (immutable
snapshot + atomic `curated` swap, previous snapshots retained);
`tests/test_etl.sh`; `Dockerfile:9-17,40-41`;
`.github/workflows/ci.yml:298-313`.)

### Import admission

The importer resolves the supplied directory to its physical published target
once (`import.sh:72-75`). All required-file checks, counts, hashes and chunk
reads use that target; an atomic replacement of `curated` affects only the
next run. Ordinary directories (including paths with spaces) also work.
Keep the selected directory and its files immutable and retained until the
run finishes; this does not protect against in-place edits or deletion.

Before any database mutation (including cursor creation), all ten TSVs must
be regular, readable and nonempty (`import.sh:111-119`). A missing, unreadable
or empty later file therefore leaves entities, cursors, DLQ and replication
state untouched. This is admission, **not whole-import atomicity**: subsequent
row validation, duplicate detection or runtime failures can still leave earlier
chunks committed, with existing resume/DLQ and final-track-prune semantics.

Optional `DUMP_SEQ` must contain only ASCII decimal digits and fit PostgreSQL
`INTEGER`, **0..2147483647** (`import.sh:121-134`). Leading zeros are normalized
before a bounded range check, so arbitrarily long digit strings cannot wrap
Bash arithmetic. Invalid input is refused before writes. Unset or empty leaves
replication state alone; zero, equal and older sequences never seed backwards.
A valid forward sequence seeds only after every table finishes (`import.sh:674-687`).

The ETL's positional mapping follows the official
[MusicBrainz `CreateTables.sql`](https://github.com/metabrainz/musicbrainz-server/blob/master/admin/sql/CreateTables.sql)
field order; use an extract from the matching schema revision. Derivations:

- Artist `type` resolves the nullable `artist.type` through `artist_type.name`;
  `country` resolves `artist.area` to its `iso_3166_1.code` (otherwise NULL).
- Release groups carry the group name, the first `artist_credit_name` artist
  by ascending position (otherwise NULL), and the primary type name.
  Release `artist_mbid` uses the first `artist_credit_name` by ascending position
  for the release's artist credit, plus the group's MBID as
  `release_group_mbid`. `release_type` resolves
  `release_group.type` through `release_group_primary_type.name` (otherwise
  NULL). `year` is the minimum non-NULL raw `date_year` across release events
  in `release_country` and `release_unknown_country` (otherwise NULL).
  (Sources: `etl.py:15-67` (pinned positions/field counts), `:435-472`
  (artist/release-group/release queries).)
- `caa_url` is emitted as
  `https://coverartarchive.org/release/<release-mbid>/front-250`; it is a
  deterministic Cover Art Archive endpoint, not proof artwork exists.
  (Sources: `etl.py:469-475` (row transform).)
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
  validator poison (`negative_position`). Parent lookups use the
  validator's normalized IDs while the DLQ keeps the exact original dump
  bytes. (Sources: `etl.py:476-517`
  (canonical recording, membership rows); `migrations/0016_mb_track.sql`;
  `import.sh:538-545` (FK specs);
  `crates/mb-import-validate/src/lib.rs` (`Position` check).)
- Relationship slices keep only allowlisted link types, resolved from their
  pinned GIDs (artist `youtube` / `youtube music` / `free streaming` /
  `streaming`, recording `free streaming`, recording-recording
  `music video`); a dump missing any pinned GID fails closed, as does a
  link input that projects zero allowlisted links. `mbdump-url.tsv` holds
  only URLs targeted by kept links — the upstream `url` table is far larger
  than the slice. `recording_url` rows carry `is_video` (`true` only when
  the link carries the pinned `video` attribute; ANY video flag wins for a
  repeated triple, so the curated `(recording, url, link)` key can never
  duplicate). `recording_music_video` rows are undirected pairs in
  canonical `(lowercased GID)` order, so one link is one row whichever side
  the dump lists first. ISRCs are deduplicated and uppercased to the mirror
  CHECK's canonical 12-character form. (Sources: `etl.py:70-84` (pinned
  GIDs), `:277-321` (allowlist resolution, kept-link set, empty-slice
  refusal), `:519-586` (url, artist/recording-url, music-video and ISRC
  projections); `migrations/0030_mb_mirror_w6b.sql`.)
- Orphan references fail the ETL before any curated file is written:
  artist type/area/country, artist-credit artists, group primary types,
  release groups/credits/years, track mediums/recordings, medium releases,
  link attribute types, and every relationship endpoint (artist/recording
  URLs, music-video sides, ISRC recordings). (Sources: `etl.py:386-431`
  (reference checks); `tests/test_etl_reference_graphs.sh`.)
- Every row gets the single supplied `DUMP_TS`; no row-specific time is created.
  Missing inputs, wrong field counts, and non-empty input that projects
  to zero rows (or a row-count mismatch on the exact projections) fail closed.
  (Sources: `etl.py:125-141` (loads), `:188-196,216` (`DUMP_TS`),
  `:596-610` (fail-closed projections).)
- Snapshots curated before `mb_track` have no `mbdump-track.tsv`, and
  snapshots curated before the W6b slice lack the six newer files: re-run
  the ETL for such a dump instead of pointing the new `import.sh` at it
  (the import refuses the incomplete curated set before any database write).
  (Sources: `import.sh:111-119` (all ten files preflighted).)

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
`negative_duration`, `negative_position`, `bad_isrc` (non-canonical ISRC),
`bad_bool` (anything but literal `true`/`false`), `invalid_encoding`; the
loader files FK orphans — a row whose parent was poisoned or never loaded
(release checks artist and group, track and the relationship/ISRC tables
check every parent) — as `fk_missing_artist` / `fk_missing_release_group` /
`fk_missing_release` / `fk_missing_recording` / `fk_missing_url` instead of
aborting the chunk. (Sources: `crates/mb-import-validate/src/lib.rs:98-114`
(reason codes), `:283-293` (`bad_escape` rule);
`crates/mb-import-validate/src/main.rs:129` (`invalid_encoding` on
non-UTF-8 bytes); `import.sh:526-618`
(FK orphan split); `migrations/0014_mb_import_resume_dlq.sql`;
`migrations/0001_mb_mirror.sql:57-65`.)

## Build and test

Build from the repository root (the validator stage needs the workspace —
it copies the full workspace and builds the single validator package):

```sh
docker build -f containers/mb-import/Dockerfile .
```

(Sources: `Dockerfile:19-25` (validator stage), `:29-49` (runtime stage).)

The runtime stage runs the jobs as the non-root `postgres` user (`Dockerfile:43-46`).

The mock-only admission suite needs Bash, Python 3 and standard shell tools,
not Cargo, psql, credentials or a database:

```sh
bash containers/mb-import/tests/test_input_admission.sh
```

Its 44 cases capture SQL attempts and payloads with command doubles: deterministic
publication switches (A during the first run, B on the next), all-file admission,
sequence boundaries, ordinary directories, directory-shaped TSV refusal,
rerun/resume and poison/FK routing.
Run unprivileged so unreadable-file controls execute rather than skip. It does
not execute PostgreSQL; the existing integration suite remains the DB-semantics
control. CI runs both (`.github/workflows/ci.yml`, `mb-import-etl` and `mb-import`).

`test-import.sh` runs the integration suite (chunked import, kill-resume,
same-dump rerun, newer-dump upsert, DLQ, FK orphans, all-poison and
dupe/cursor-ahead refusals, track repoint/prune, interrupted multi-chunk
resume — suites 1-12 cover all ten tables, including the relationship/ISRC
generators and the newer/older-changed-dump and split-fixture suites) against
a scratch database: (Sources: `test-import.sh:10-15` (contract/refusal
guard), `:111-665` (suites 1-12);
`.github/workflows/ci.yml:746-790`.)

Run it in the shipped image, as CI does (the image carries `mb-validate`):

```sh
docker build -f containers/mb-import/Dockerfile -t mb-import:ci .
docker run --rm --network host \
  -e DATABASE_URL=<scratch-url> -e ALLOW_TEST_TRUNCATE=1 \
  -v "$PWD/containers/mb-import/test-import.sh:/job/test-import.sh:ro" \
  mb-import:ci bash /job/test-import.sh
```

The suite truncates all ten mirror tables, so `ALLOW_TEST_TRUNCATE=1` is
mandatory and the target must be scratch — never staging or production.
(Sources: `test-import.sh:11-15` (refusal guard), `:47-51` (truncate + state
reset).) Unit tests for the validator run with the workspace: `cargo test`.
(Sources: `Cargo.toml:2` (workspace member);
`crates/mb-import-validate/src/lib.rs:667-1155` (unit tests);
`.github/workflows/ci.yml:51` (`cargo test --workspace`).)

## Secrets

`DATABASE_URL` comes from the secret store — never committed, never
logged by the scripts under normal (non-xtrace) operation. The import job
(`import.sh`) reads it from env only: `pg-env.sh` translates it into `PG*`
variables and unsets it, so `psql` is invoked with no connection string in
its argv (argv hygiene is pinned by `tests/test_argv_hygiene.sh`, which
also pins that the retired `replicate.sh` spawns no child at all). Do not
run the scripts under `bash -x` / `SHELLOPTS=xtrace`: tracing expands the
`: "${DATABASE_URL:?...}"` presence check and `pg-env.sh`'s `eval`,
printing the secret to stderr. (Sources: `import.sh:4-6,67-76,123-127`;
`replicate.sh:1-11` (retired, reads nothing); `pg-env.sh:1-12,287-297`;
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
place (cursor PK, DLQ unique, `table_name` checks), and 0030 adds its six
tables the same way (extending the cursor/DLQ `table_name` checks in
place). (Sources: `import.sh:214-239`
(prune, DLQ gap), `:618-636` (single transaction, forward-only
guard, final-chunk prune); `test-import.sh:555-565` (older dump leaves rows alone);
`migrations/0016_mb_track.sql:38-43` (check swaps);
`migrations/0014_mb_import_resume_dlq.sql:28-29,46-49` (PK/unique swaps);
`migrations/0030_mb_mirror_w6b.sql:103-114` (`table_name` check extension).)
