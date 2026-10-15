#!/usr/bin/env bash
# mbdump dumps -> Postgres COPY into nntunev2-mb (idempotent, resumable).
# Usage: DATABASE_URL=<url> [DUMP_SEQ=<n>] [MB_IMPORT_CHUNK_LINES=<n>] ./import.sh <dump-dir>
# Never echo $DATABASE_URL; pass it only via env/secret store.
#
# Kill-safe chunking: each dump file is applied in line-range chunks
# (default 2000 lines), one transaction per chunk. The committed line
# number is recorded in mb_import_cursor, so kill -9 mid-import resumes
# after the last committed chunk instead of restarting the file. Rerunning
# over the same dump inserts zero dupes: chunks at or below the cursor are
# skipped, and the upsert carries the forward-only updated_at guard (an
# older dump never rolls back a row replication moved ahead).
#
# Poison rows never abort the import: the mb-validate pre-pass checks every
# line against the typed upsert contract (uuid/int/timestamp shapes, COPY
# escapes, canonical dump layout) and poison lines land in mb_import_dlq
# with reason + raw line for the operator. A duplicate MBID inside one
# chunk is fatal (the dump contradicts itself); the same MBID in two
# chunks is fatal too, so a mid-file edit between runs can never silently
# double-apply. FK order is artist -> release -> recording: every chunk of
# a later table only references rows an earlier table already committed,
# and immediate FK checks never see a half-chunk (a failed chunk rolls
# back whole).
#
# DUMP_SEQ (optional): replication sequence the dump corresponds to. When
# set, mb_import_state.last_packet is seeded forward-only (only when
# DUMP_SEQ > current last_packet) so the hourly replicate.sh gate
# (PACKET_SEQ = last_packet + 1) accepts the first real packet. The seed
# never touches last_applied_at/lag_seconds (apply-step columns) and an
# older dump never rewinds the cursor. When unset, state is left untouched
# and the operator must seed it before replication (see README).
#
# Input contract (curated TSVs, NOT raw MusicBrainz dumps):
#   <dump-dir>/mbdump-artist.tsv     mb_artist    (mbid, name, sort_name, type, country, updated_at)
#   <dump-dir>/mbdump-release.tsv    mb_release   (mbid, artist_mbid, title, release_type, year, caa_url, updated_at)
#   <dump-dir>/mbdump-recording.tsv  mb_recording (mbid, release_mbid, title, track_no, duration_ms, updated_at)
# Produce them from an official mbdump/ extract (mbdump/artist, mbdump/release,
# mbdump/recording with the full upstream column layout) by projecting the
# curated columns in dump order, e.g. with a small ETL step that SELECTs the
# columns above and writes tab-separated rows. Missing input is fatal: a
# silent skip would report "import: done" with zero rows loaded.
set -euo pipefail

if [[ "${1:-}" == "--help" || $# -lt 1 ]]; then
  cat <<'USAGE'
Usage: DATABASE_URL=<postgres-url> ./import.sh <dump-dir>

Imports curated mbdump TSV dumps (artist, release, recording) in kill-safe
chunks: validate -> COPY -> typed upsert -> cursor advance, one transaction
per chunk, poison rows to mb_import_dlq. A rerun resumes from
mb_import_cursor and inserts zero dupes. See the header comment for the
curated TSV contract and how to project it from official mbdump/ extracts.
Requires: psql and mb-validate on PATH (MB_VALIDATE_BIN overrides),
DATABASE_URL in env (never as argv).
USAGE
  exit 0
fi

dump_dir="$1"
: "${DATABASE_URL:?set DATABASE_URL via secret store, never on the command line}"

VALIDATE_BIN="${MB_VALIDATE_BIN:-mb-validate}"
CHUNK_LINES="${MB_IMPORT_CHUNK_LINES:-2000}"
if [[ ! "$CHUNK_LINES" =~ ^[0-9]+$ ]] || (( 10#$CHUNK_LINES < 1 )); then
  echo "import: refusing MB_IMPORT_CHUNK_LINES='$CHUNK_LINES' (positive integer required)" >&2
  exit 1
fi
if [[ "$VALIDATE_BIN" == */* ]]; then
  [[ -x "$VALIDATE_BIN" ]] || { echo "import: mb-validate not executable at $VALIDATE_BIN" >&2; exit 1; }
else
  command -v "$VALIDATE_BIN" >/dev/null || { echo "import: mb-validate not on PATH (set MB_VALIDATE_BIN)" >&2; exit 1; }
fi
command -v psql >/dev/null || { echo "import: psql not on PATH" >&2; exit 1; }
command -v awk >/dev/null || { echo "import: awk not on PATH" >&2; exit 1; }

# Chunk scratch lives here for the whole run (one rm at the end). A kill -9
# skips the EXIT trap and leaks this directory; chunk files are disposable
# (the cursor is the resume state, never these files) and the platform GCs
# TMPDIR, so a leak is untracked clutter, never corruption.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

for table in artist release recording; do
  tsv="$dump_dir/mbdump-$table.tsv"
  if [[ ! -f "$tsv" ]]; then
    echo "import: missing $tsv (expected curated TSV; see header comment)" >&2
    exit 1
  fi
  case "$table" in
    artist)
      columns="mbid, name, sort_name, \"type\", country, updated_at"
      upsert="name = EXCLUDED.name, sort_name = EXCLUDED.sort_name, \"type\" = EXCLUDED.\"type\", country = EXCLUDED.country, updated_at = EXCLUDED.updated_at"
      ;;
    release)
      columns="mbid, artist_mbid, title, release_type, year, caa_url, updated_at"
      upsert="artist_mbid = EXCLUDED.artist_mbid, title = EXCLUDED.title, release_type = EXCLUDED.release_type, year = EXCLUDED.year, caa_url = EXCLUDED.caa_url, updated_at = EXCLUDED.updated_at"
      ;;
    recording)
      columns="mbid, release_mbid, title, track_no, duration_ms, updated_at"
      upsert="release_mbid = EXCLUDED.release_mbid, title = EXCLUDED.title, track_no = EXCLUDED.track_no, duration_ms = EXCLUDED.duration_ms, updated_at = EXCLUDED.updated_at"
      ;;
  esac
  # awk NR counts a trailing partial line; the validator counts the same
  # way, so cursor line numbers and sed ranges agree on every file shape.
  total="$(awk 'END { print NR+0 }' "$tsv")"
  completed="$(psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -tAX -c "SELECT lines_done FROM mb_import_cursor WHERE table_name = '$table';")"
  if [[ ! "$completed" =~ ^[0-9]+$ ]]; then
    echo "import: unexpected lines_done '$completed' for $table in mb_import_cursor (migration 0003 missing?)" >&2
    exit 1
  fi
  if (( 10#$completed > 10#$total )); then
    echo "import: cursor for $table is at line $completed but $tsv has $total lines; the dump shrank or was replaced mid-run: refusing (reset mb_import_cursor deliberately, never automatically)" >&2
    exit 1
  fi
  if (( 10#$completed == 10#$total )); then
    echo "import: $table already complete ($total/$total lines); skipping"
    continue
  fi
  echo "import: loading $table from $tsv (resuming at line $((10#$completed + 1)) of $total)"
  while true; do
    completed="$(psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -tAX -c "SELECT lines_done FROM mb_import_cursor WHERE table_name = '$table';")"
    if (( 10#$completed >= 10#$total )); then
      break
    fi
    range="$("$VALIDATE_BIN" --plan --completed "$completed" --total "$total" --size "$CHUNK_LINES" | head -n 1)"
    if [[ -z "$range" ]]; then
      break
    fi
    start="${range%-*}"
    end="${range#*-}"
    offset="$((10#$start - 1))"
    want="$((10#$end - 10#$start + 1))"
    sed -n "${start},${end}p" "$tsv" >"$work/chunk.tsv"
    summary="$("$VALIDATE_BIN" --table "$table" --in "$work/chunk.tsv" --valid-out "$work/valid.tsv" --poison-out "$work/poison.tsv" --line-offset "$offset")"
    valid_n="${summary#valid=}"
    valid_n="${valid_n%% *}"
    poison_n="${summary##*poison=}"
    if [[ ! "$valid_n" =~ ^[0-9]+$ || ! "$poison_n" =~ ^[0-9]+$ ]]; then
      echo "import: cannot parse validator summary '$summary' for $table lines $range" >&2
      exit 1
    fi
    if (( 10#$valid_n + 10#$poison_n != 10#$want )); then
      echo "import: chunk $table lines $range yielded $((10#$valid_n + 10#$poison_n)) of $want lines; the dump changed under the import: refusing" >&2
      exit 1
    fi
    dupes="$(awk -F'\t' '{ print $1 }' "$work/valid.tsv" | sort | uniq -d | head -n 5)"
    if [[ -n "$dupes" ]]; then
      echo "import: duplicate MBID inside $table chunk lines $range (dump contradicts itself; fix the dump, cursor stays at $completed):" >&2
      echo "$dupes" >&2
      exit 1
    fi
    # A repeated MBID in an EARLIER chunk would upsert onto the already
    # loaded row, so the chunk's line count and row count legitimately
    # differ. Refuse up front with the offending keys instead of letting
    # the post-commit row-count check fail on committed data: the dump
    # must carry each MBID once (official extracts do), and a repeated key
    # means the dump was regenerated or hand-edited mid-run.
    if (( 10#$valid_n > 0 )); then
      awk -F'\t' '{ print $1 }' "$work/valid.tsv" >"$work/mbids.tsv"
      # No explicit transaction here, so no ON COMMIT DROP: each statement
      # autocommits, which would drop the temp table before COPY runs. The
      # table dies with the session instead.
      overlap="$(psql "$DATABASE_URL" -q -v ON_ERROR_STOP=1 -tAX <<SQL
CREATE TEMP TABLE want (mbid uuid);
\copy want FROM '$work/mbids.tsv' WITH (FORMAT text)
SELECT CASE WHEN count(*) = 0 THEN '' ELSE count(*) || ' already loaded, e.g. ' || string_agg(mbid::text, ', ') END
FROM (SELECT w.mbid FROM want w JOIN mb_${table} t USING (mbid) LIMIT 5) s;
SQL
)"
      if [[ -n "$overlap" ]]; then
        echo "import: $table chunk lines $range repeats MBID(s) from earlier lines ($overlap); fix the dump, cursor stays at $completed" >&2
        exit 1
      fi
    fi
    # Single psql session + single transaction: the TEMP stage tables are
    # session-scoped, so create, COPY, upsert, DLQ load and cursor advance
    # must share one connection. The WHERE guard makes reruns move rows
    # forward only: an older dump never rolls back a row that replication
    # (or a newer dump) moved ahead. The cursor advance is conditional on
    # the value read above: a concurrent importer moving it first makes
    # RETURNING come back empty and this run stops instead of double-
    # applying. DLQ inserts ignore conflicts so a replayed chunk (kill
    # between COMMIT and cursor read) never double-logs poison.
    # -q keeps stdout to just the RETURNING value: without it every command
    # tag (BEGIN/COPY/INSERT/...) lands in the capture and the comparison
    # below misfires.
    advanced="$(psql "$DATABASE_URL" -q -v ON_ERROR_STOP=1 -tAX <<SQL
BEGIN;
CREATE TEMP TABLE stage (LIKE mb_${table}) ON COMMIT DROP;
\copy stage ($columns, line_no) FROM '$work/valid.tsv' WITH (FORMAT text)
INSERT INTO mb_${table} ($columns, line_no) SELECT $columns, line_no FROM stage
ON CONFLICT (mbid) DO UPDATE SET $upsert
WHERE mb_${table}.updated_at < EXCLUDED.updated_at;
CREATE TEMP TABLE poison (reason text, line_no bigint, raw text) ON COMMIT DROP;
\copy poison FROM '$work/poison.tsv' WITH (FORMAT text)
INSERT INTO mb_import_dlq (table_name, line_no, reason, raw_line)
SELECT '$table', line_no, reason, raw FROM poison
ON CONFLICT (table_name, line_no) DO NOTHING;
UPDATE mb_import_cursor SET lines_done = $end, updated_at = now()
WHERE table_name = '$table' AND lines_done = $completed
RETURNING lines_done;
COMMIT;
SQL
)"
    if [[ "$advanced" != "$end" ]]; then
      echo "import: cursor for $table moved under this run (expected $end, got '$advanced'); stopping without damage" >&2
      exit 1
    fi
    committed="$(psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -tAX -c "SELECT count(*) FROM mb_${table} WHERE line_no BETWEEN $start AND $end;")"
    if [[ "$committed" != "$valid_n" ]]; then
      echo "import: chunk $table lines $range committed $committed rows, validator passed $valid_n; stopping (cursor is at $end, rerun replays safely)" >&2
      exit 1
    fi
    echo "import: $table lines $start-$end committed ($valid_n rows, $poison_n poison)"
  done
done

# Record the dump's replication sequence so replicate.sh accepts the first
# real packet. Forward-only seed: advances last_packet only when
# DUMP_SEQ > current last_packet, and never touches last_applied_at /
# lag_seconds (those are written only by the replicate apply step). An
# older-dump rerun (DUMP_SEQ <= last_packet) warns and leaves state
# untouched so the cursor never rewinds. Unset DUMP_SEQ leaves state
# untouched (fresh DBs stay at the migration seed 0); the operator then
# seeds last_packet before replication.
if [[ -n "${DUMP_SEQ:-}" ]]; then
  if [[ ! "$DUMP_SEQ" =~ ^[0-9]+$ ]]; then
    echo "import: refusing non-integer DUMP_SEQ '$DUMP_SEQ'" >&2
    exit 1
  fi
  current_last_packet="$(psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -tAX -c "SELECT last_packet FROM mb_import_state WHERE id = 1;")"
  if [[ ! "$current_last_packet" =~ ^[0-9]+$ ]]; then
    echo "import: unexpected last_packet '$current_last_packet' in mb_import_state" >&2
    exit 1
  fi
  if (( 10#$DUMP_SEQ <= 10#$current_last_packet )); then
    echo "import: DUMP_SEQ=$DUMP_SEQ <= current last_packet=$current_last_packet; leaving state untouched (older dump never rewinds the replication cursor)" >&2
  else
    psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "UPDATE mb_import_state SET last_packet = $DUMP_SEQ, updated_at = now() WHERE id = 1 AND last_packet < $DUMP_SEQ;"
    echo "import: seeded mb_import_state.last_packet=$DUMP_SEQ"
  fi
else
  echo "import: DUMP_SEQ unset; leaving mb_import_state untouched (seed last_packet before running replicate.sh)" >&2
fi

echo "import: done"
