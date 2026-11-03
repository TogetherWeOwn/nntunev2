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
| Initial import | manual Container run | `import.sh <curated-dir>` (twenty-five `mbdump-<table>.tsv` files — the ten base tables (`artist`, `release_group`, `release`, `recording`, `track`, `url`, `artist_url`, `recording_url`, `recording_music_video`, `recording_isrc`) plus the fifteen phase-2 tables (`work`, `label`, `artist_recording`, `artist_release`, `artist_work`, `recording_work`, `recording_recording`, `release_credit`, `artist_alias`, `release_alias`, `recording_alias`, `release_group_alias`, `work_alias`, `label_alias`, `release_label`), required as a group only when any one of them is present; missing base input is fatal; optional `DUMP_SEQ=<n>` seeds `mb_import_state.last_packet` forward-only; `MB_IMPORT_CHUNK_LINES=<n>` sizes chunks, default 2000) | kill-safe and idempotent: each file is keyed by its SHA-256 in `mb_import_cursor`, then applied in one-transaction chunks (validate via `mb-validate` -> `COPY` -> typed upsert -> cursor advance). The same dump resumes after its last committed chunk; a changed dump gets a fresh cursor even if it has the same or fewer lines. Poison rows land in `mb_import_dlq` per dump with reason + raw line instead of aborting the run. Duplicate valid keys within one dump — MBIDs on the entity, `url`, `work` and `label` tables, the `(artist, url, link)` / `(recording, url, link)` triples on the URL-relationship tables, the unordered recording pair on `recording_music_video`, the `(recording, isrc)` pair on `recording_isrc`, the `(a, b, link)` triple on the directed relation tables, the `(release, position)` key on `release_credit`, the `(entity, name, locale)` triple on the alias tables, the `(release, label, catalogue)` triple on `release_label`, the `(release, medium, track)` position on `mbdump-track.tsv` — or a cursor ahead of that dump are fatal; the same key in a later dump is allowed and uses the forward-only upsert (`WHERE updated_at < EXCLUDED.updated_at`). Duplicate checks COPY-decode and canonicalize keys the way the validator normalizes them (lowercase UUIDs, plain integers, literal bools, uppercase ISRCs, the music-video pair sorted like the canonical ETL ordering), so an octal/hex-escaped alias for an already-listed key is refused, not loaded beside it. `DUMP_SEQ` seeds `last_packet` only when it advances the cursor, never touches `last_applied_at`/`lag_seconds`, so an older dump never rolls back newer rows or rewinds replication. Seed `last_packet` via `DUMP_SEQ` (or manually) so per-packet lag accounting has a baseline. (Sources: `import.sh:121` (`MB_IMPORT_CHUNK_LINES`, default 2000); `:139-155,181-192` (all-file and sequence admission); `:461-462` (SHA-256 `dump_id` + cursor insert); `:463-655` (alias-aware whole-file dupe refusal); `:656-671` (cursor read/ahead/complete); `:724-756` (per-chunk dupe check); `:864-885` (one-transaction chunk); `:920-931` (`DUMP_SEQ` forward-only seed); `Dockerfile:48-49` (scheduler overrides default CMD).) |
| Replication | retired (no schedule) | `replicate.sh` refuses every invocation | Hourly Live Data Feed packets carry the NC licence variant and never enter the CC0-only mirror; product reads come from the twice-weekly CC0 core dumps via the initial-import job above. The retired script exits 2 **before** reading secrets, touching the network, or writing any state, preserving the old pending-apply refusal signal for callers that branch on it. The per-packet SLO contract in ADR 0007 (each applied packet within 2 h of publication, evidence in `mb_replication_packet.lag_secs`) stands for whenever packet application returns under a CC0-compatible feed. (Sources: `replicate.sh:1-11` (retirement, exit 2, no state); `docs/adr/0007-mb-mirror.md:24-26` (SLO); `migrations/0017_mb_replication_packet_ledger.sql`; `migrations/0001_mb_mirror.sql:57-65` (state columns).) |

## Initial dump ETL

Run `DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>` before
`import.sh <mbdump-dir>/curated`. The input directory must contain the official,
uncompressed, headerless MusicBrainz COPY files: the entity files (`artist`
with `artist_type`/`area`/`iso_3166_1` lookups, `release` with
`artist_credit_name`/`release_group`/`release_group_primary_type`/`release_country`/`release_unknown_country`,
`recording` with `medium`/`track`) plus the relationship files (`url`,
`link_type`, `link`, `link_attribute_type`, `link_attribute`,
`l_artist_url`, `l_recording_url`, `l_recording_recording`, `isrc`,
`l_artist_recording`, `l_artist_release`, `l_artist_work`,
`l_recording_work`, the six alias tables, `label`/`label_type`,
`release_label`, `work`/`work_type`, `link` (full load; the
URL/music-video slice keeps its allowlisted subset) and
`artist_credit_name`) listed in `etl.py`. The ETL writes the
twenty-five curated `mbdump-<table>.tsv` files into an
immutable snapshot and atomically swaps `<mbdump-dir>/curated` to point to it;
previous snapshots are retained. It uses the image's Python 3 and a temporary,
disk-backed SQLite projection (no external database). Run the fixture checks
with `bash containers/mb-import/tests/test_etl.sh` and
`bash containers/mb-import/tests/test_etl_reference_graphs.sh` (from the
repository root — the latter pins eight broken-reference refusal
diagnostics: missing artist_type, release credit-name rows, track
recording, medium release, phase-2 relation artist, work-alias parent,
release-label release, label type);
the Dockerfile's test stage runs them during the normal image build without
shipping fixtures in the runtime image. (Sources: `etl.sh:1-14`;
`etl.py:339-348` (`DUMP_TS` validation), `:375-380` (temp SQLite projection), `:1086-1102` (immutable
snapshot + atomic `curated` swap, previous snapshots retained);
`tests/test_etl.sh`; `Dockerfile:9-17,40-41`;
`.github/workflows/ci.yml:298-313`.)

### Import admission

The importer resolves the supplied directory to its physical published target
once (`import.sh:114-117`). All required-file checks, counts, hashes and chunk
reads use that target; an atomic replacement of `curated` affects only the
next run. Ordinary directories (including paths with spaces) also work.
Keep the selected directory and its files immutable and retained until the
run finishes; this does not protect against in-place edits or deletion.

Before any database mutation (including cursor creation), all ten base
TSVs must be regular, readable and nonempty (`import.sh:149-155`). A
missing, unreadable or empty later file therefore leaves entities,
cursors, DLQ and replication state untouched. The fifteen phase-2 TSVs
are optional as a group: a snapshot predating phase 2 (none of the
fifteen present) imports its base tables unchanged, while any snapshot
carrying at least one phase-2 file must carry all fifteen, regular,
readable and nonempty. This is admission, **not whole-import atomicity**: subsequent
row validation, duplicate detection or runtime failures can still leave earlier
chunks committed, with existing resume/DLQ and final-track-prune semantics.

Optional `DUMP_SEQ` must contain only ASCII decimal digits and fit PostgreSQL
`INTEGER`, **0..2147483647** (`import.sh:181-192`). Leading zeros are normalized
before a bounded range check, so arbitrarily long digit strings cannot wrap
Bash arithmetic. Invalid input is refused before writes. Unset or empty leaves
replication state alone; zero, equal and older sequences never seed backwards.
A valid forward sequence seeds only after every table finishes (`import.sh:920-931`).

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
  (Sources: `etl.py:15-67` (pinned positions/field counts), `:739-773`
  (artist/release-group/release queries).)
- `caa_url` is emitted as
  `https://coverartarchive.org/release/<release-mbid>/front-250`; it is a
  deterministic Cover Art Archive endpoint, not proof artwork exists.
  (Sources: `etl.py:770-775` (row transform).)
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
  bytes. (Sources: `etl.py:777-820`
  (canonical recording, membership rows); `migrations/0016_mb_track.sql`;
  `import.sh:775` (FK specs);
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
  CHECK's canonical 12-character form. (Sources: `etl.py:113-133` (pinned
  GIDs), `:450-499` (allowlist resolution, kept-link set, empty-slice
  refusal), `:823-893` (url, artist/recording-url, music-video and ISRC
  projections); `migrations/0030_mb_mirror_w6b.sql`.)
- Orphan references fail the ETL before any curated file is written:
  artist type/area/country, artist-credit artists, group primary types,
  release groups/credits/years, track mediums/recordings, medium releases,
  link attribute types, and every relationship endpoint (artist/recording
  URLs, music-video sides, ISRC recordings). (Sources: `etl.py:604-662`
  (reference checks); `tests/test_etl_reference_graphs.sh`.)
- Every row gets the single supplied `DUMP_TS`; no row-specific time is created.
  Missing inputs, wrong field counts, and non-empty input that projects
  to zero rows (or a row-count mismatch on the exact projections) fail closed.
  (Sources: `etl.py:260-293` (loads), `:351-368` (`DUMP_TS`),
  `:1044-1056` (fail-closed projections).)
- Snapshots curated before `mb_track` have no `mbdump-track.tsv`, and
  snapshots curated before the W6b slice lack the six newer files: re-run
  the ETL for such a dump instead of pointing the new `import.sh` at it
  (the import refuses the incomplete curated set before any database write).
  Snapshots curated before phase 2 carry none of the fifteen phase-2
  files and still import their base tables; a snapshot carrying any
  phase-2 file must carry all fifteen.
  (Sources: `import.sh:149-155,166-180` (base-ten plus phase-2-group preflight).)

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
aborting the chunk. (Sources: `crates/mb-import-validate/src/lib.rs:173-189`
(reason codes), `:409-418` (`bad_escape` rule);
`crates/mb-import-validate/src/main.rs:129` (`invalid_encoding` on
non-UTF-8 bytes); `import.sh:757-861`
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
resume, relationship/alias/credit/label-edge prune — suites 1-14 cover
all twenty-five tables, including the phase-2 generators, the FK-orphan
routing across every parent, and the newer/older-changed-dump and
split-fixture suites) against
a scratch database: (Sources: `test-import.sh:10-15` (contract/refusal
guard), `:suites 1-14`;
`.github/workflows/ci.yml:746-790`.)

Run it in the shipped image, as CI does (the image carries `mb-validate`):

```sh
docker build -f containers/mb-import/Dockerfile -t mb-import:ci .
docker run --rm --network host \
  -e DATABASE_URL=<scratch-url> -e ALLOW_TEST_TRUNCATE=1 \
  -v "$PWD/containers/mb-import/test-import.sh:/job/test-import.sh:ro" \
  mb-import:ci bash /job/test-import.sh
```

The suite truncates all twenty-five mirror tables, so `ALLOW_TEST_TRUNCATE=1` is
mandatory and the target must be scratch — never staging or production.
(Sources: `test-import.sh:11-15` (refusal guard), `:47-51` (truncate + state
reset).) Unit tests for the validator run with the workspace: `cargo test`.
(Sources: `Cargo.toml:2` (workspace member);
`crates/mb-import-validate/src/lib.rs:799-1389` (unit tests);
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
printing the secret to stderr. (Sources: `import.sh:4-6,118,197-200`;
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
place), as does 0035 with its fifteen phase-2 tables (swapped `NOT VALID`
inside the transaction, validated after `COMMIT`, so the swap never scans
the cursor/DLQ tables under lock). (Sources: `import.sh:421-442`
(prune, DLQ gap), `:864-882` (single transaction, forward-only
guard, final-chunk prune); `test-import.sh:555-565` (older dump leaves rows alone);
`migrations/0016_mb_track.sql:38-43` (check swaps);
`migrations/0014_mb_import_resume_dlq.sql:28-29,46-49` (PK/unique swaps);
`migrations/0030_mb_mirror_w6b.sql:103-114` (`table_name` check extension);
`migrations/0035_mb_mirror_phase2_relations.sql:242-259` (`table_name` check extension),
`:266-267` (post-`COMMIT` validation).)
