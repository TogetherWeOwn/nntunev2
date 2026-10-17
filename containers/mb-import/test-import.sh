#!/usr/bin/env bash
# Integration test for import.sh: chunked import, kill-resume, same-dump
# rerun, newer-dump upsert, DLQ query, and refusal guards. Needs psql,
# python3 and mb-validate
# on PATH (or MB_VALIDATE_BIN), plus DATABASE_URL to a SCRATCH database.
# DESTRUCTIVE: truncates mb_artist/mb_release/mb_recording/mb_track/mb_import_dlq and
# resets mb_import_cursor/mb_import_state, so it refuses to run unless
# ALLOW_TEST_TRUNCATE=1 is set. Never point it at staging or production.
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL to a scratch database (never staging/prod)}"
[[ "${ALLOW_TEST_TRUNCATE:-}" == "1" ]] || {
  echo "test-import: refusing without ALLOW_TEST_TRUNCATE=1 (this test truncates mirror tables)" >&2
  exit 2
}
command -v psql >/dev/null || { echo "test-import: psql not on PATH" >&2; exit 2; }
command -v python3 >/dev/null || { echo "test-import: python3 not on PATH" >&2; exit 2; }

SELF_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VALIDATE_BIN="${MB_VALIDATE_BIN:-mb-validate}"
if [[ "$VALIDATE_BIN" == */* ]]; then
  [[ -x "$VALIDATE_BIN" ]] || { echo "test-import: mb-validate not executable at $VALIDATE_BIN" >&2; exit 2; }
else
  command -v "$VALIDATE_BIN" >/dev/null || { echo "test-import: mb-validate not on PATH (set MB_VALIDATE_BIN)" >&2; exit 2; }
fi
export MB_VALIDATE_BIN="$VALIDATE_BIN"
export MB_IMPORT_CHUNK_LINES=25

db() { psql "$DATABASE_URL" -q -v ON_ERROR_STOP=1 -tAX "$@"; }
db_show() { psql "$DATABASE_URL" -v ON_ERROR_STOP=1 "$@"; }
dump_id() { sha256sum "$1" | cut -d ' ' -f 1; }
cursor() { db -c "SELECT lines_done FROM mb_import_cursor WHERE table_name='$1' AND dump_id='$(dump_id "$2")';"; }

fail=0
check() {
  local desc="$1" want="$2" got="$3"
  if [[ "$got" == "$want" ]]; then
    echo "ok: $desc ($got)"
  else
    echo "FAIL: $desc (want $want, got $got)" >&2
    fail=1
  fi
}

reset_db() {
  db -c "TRUNCATE mb_track, mb_recording, mb_release, mb_artist, mb_import_dlq;" \
     -c "UPDATE mb_import_cursor SET lines_done = 0;" \
     -c "UPDATE mb_import_state SET last_packet = 0, last_applied_at = NULL, lag_seconds = NULL;" >/dev/null
}

FIX="$SELF_DIR/.test-fixtures"
rm -rf "$FIX"
mkdir -p "$FIX"
python3 - "$FIX" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
N_ART, N_REL, N_REC = 120, 60, 60
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    for i in range(1, N_ART + 1):
        f.write(f'{eid(i)}\tBand {i}\t{i}, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write('not-a-uuid\tBad\tBad\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write(f'{eid(9001)}\tBad TS\tBad TS\tGroup\tUS\tnot-a-date\n')
    f.write(f'{eid(9002)}\tShort\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid(100000+i)}\t{eid((i % N_ART) + 1)}\tAlbum {i}\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
    f.write('also-bad\t\\N\tBad\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(200000+i)}\t{eid(100000 + (i % N_REL) + 1)}\tTrack {i}\t{(i % 12) + 1}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(100000 + (i % N_REL) + 1)}\t{eid(200000+i)}\t1\t{(i % 12) + 1}\t2024-05-03T00:00:00Z\n')
    # One recording also appears on a second release (medium 2): the
    # membership file, unlike the canonical recording row, keeps both.
    f.write(f'{eid(100001)}\t{eid(200001)}\t2\t1\t2024-05-03T00:00:00Z\n')
EOF

echo "--- target: $(db -c "SELECT current_database() || chr(47) || current_user || '@' || coalesce(inet_server_addr()::text, 'socket');")"

echo "=== 1. full import: counts, cursor, DLQ ==="
reset_db
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
check "artists loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "releases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
check "recordings loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
check "tracks loaded" "61" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "shared recording keeps both memberships" "2" \
  "$(db -c "SELECT count(*) FROM mb_track WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
check "artist cursor" "123" "$(cursor artist "$FIX/mbdump-artist.tsv")"
check "release cursor" "61" "$(cursor release "$FIX/mbdump-release.tsv")"
check "recording cursor" "60" "$(cursor recording "$FIX/mbdump-recording.tsv")"
check "track cursor" "61" "$(cursor track "$FIX/mbdump-track.tsv")"
check "dlq rows" "4" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "dlq reasons" "bad_uuid,bad_timestamp,column_count,bad_uuid" \
  "$(db -c "SELECT string_agg(reason, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "dlq line_nos" "121,122,123,61" \
  "$(db -c "SELECT string_agg(line_no::text, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "dlq raw round-trips raw key" "not-a-uuid" \
  "$(db -c "SELECT split_part(raw_line, chr(9), 1) FROM mb_import_dlq WHERE reason='bad_uuid' AND table_name='artist';")"
check "artist dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_artist GROUP BY mbid HAVING count(*)>1) d;')"
check "artist line range" "1|120|120" \
  "$(db -c 'SELECT min(line_no) || CHR(124) || max(line_no) || CHR(124) || count(*) FROM mb_artist;')"
check "operator DLQ query works" "1" \
  "$(db -c "SELECT count(*) FROM mb_import_dlq WHERE table_name='artist' AND reason='bad_uuid';")"

echo "=== 2. rerun over same dump: zero dupes, zero movement ==="
before="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
after="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
check "rerun changes nothing" "$before" "$after"

echo "=== 3. same-sized newer dump upserts rows and isolates cursor/DLQ ==="
NEWER="$FIX-newer"
rm -rf "$NEWER"
cp -R "$FIX" "$NEWER"
python3 - "$NEWER/mbdump-artist.tsv" <<'EOF'
import sys
from pathlib import Path
p = Path(sys.argv[1])
lines = p.read_text().splitlines()
fields = lines[0].split('\t')
fields[1] = 'Band 9'
fields[5] = '2025-05-01T12:00:00Z'
lines[0] = '\t'.join(fields)
p.write_text('\n'.join(lines) + '\n')
EOF
[[ "$(wc -c <"$FIX/mbdump-artist.tsv")" == "$(wc -c <"$NEWER/mbdump-artist.tsv")" ]]
bash "$SELF_DIR/import.sh" "$NEWER" >/dev/null
check "new dump updates existing artist" "Band 9|2025-05-01 12:00:00+00"   "$(db -c "SELECT name || '|' || updated_at FROM mb_artist WHERE mbid='00000000-0000-0000-0000-000000000001';")"
check "new dump preserves table row counts" "120|60|60|61"   "$(db -c 'SELECT (SELECT count(*) FROM mb_artist) || chr(124) || (SELECT count(*) FROM mb_release) || chr(124) || (SELECT count(*) FROM mb_recording) || chr(124) || (SELECT count(*) FROM mb_track);')"
check "new dump starts an independent cursor" "123" "$(cursor artist "$NEWER/mbdump-artist.tsv")"
check "DLQ retains poison from both dumps" "7" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "same line poison is keyed by dump" "2" \
  "$(db -c "SELECT count(DISTINCT dump_id) FROM mb_import_dlq WHERE table_name='artist' AND line_no=121;")"

echo "=== 4. kill -9 mid-import resumes from cursor ==="
reset_db
# Own process group (`set -m`): the kill below must take the in-flight psql
# chunk transaction with the bash. Killing only the bash orphans the psql
# child; if it commits after the resume run reads the cursor, the resume's
# conditional cursor UPDATE matches nothing and the suite fails spuriously.
# Monitor mode puts this background job in its own group (pgid == $!, set at
# fork and kept after `set +m`), so the group kill below is deterministic —
# unlike setsid, which forks (and strands $! on a dead parent) whenever the
# parent shell already gave the job its own group.
set -m
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null 2>&1 &
imp=$!
set +m
killed_at=""
for _ in $(seq 1 1200); do
  sleep 0.05
  cur="$(cursor artist "$FIX/mbdump-artist.tsv")"
  if [[ "$cur" =~ ^[0-9]+$ ]] && (( 10#$cur >= 25 )); then
    killed_at="$cur"
    break
  fi
done
if [[ -z "$killed_at" ]]; then
  echo "FAIL: import never reached line 25 (cannot test kill-resume)" >&2
  kill -- "-$imp" 2>/dev/null || true
  wait "$imp" 2>/dev/null || true
  fail=1
# A resume test that never interrupts proves nothing: a completed import +
# idempotent rerun (or a mutant that ignores the cursor) passes final-count
# checks identically. Require a genuine mid-flight kill: the importer is
# still alive, at least one table is incomplete, and the process dies by
# signal (not exit 0). Then prove the resume did real work: rows were
# missing before it and complete after. (The kill may land after the artist
# table finished as long as later tables are still loading — resume must
# skip the completed artist chunks with zero dupes all the same.)
elif ! kill -0 "$imp" 2>/dev/null; then
  echo "FAIL: importer already exited before kill -9 (cursor=$killed_at); resume untested" >&2
  wait "$imp" 2>/dev/null || true
  fail=1
else
  rel_at="$(cursor release "$FIX/mbdump-release.tsv")"
  rec_at="$(cursor recording "$FIX/mbdump-recording.tsv")"
  trk_at="$(cursor track "$FIX/mbdump-track.tsv")"
  if [[ "$killed_at" == "123" && "$rel_at" == "61" && "$rec_at" == "60" && "$trk_at" == "61" ]]; then
    echo "FAIL: import completed before kill -9 (nothing to resume)" >&2
    kill -- "-$imp" 2>/dev/null || true
    wait "$imp" 2>/dev/null || true
    fail=1
  else
    # Group kill: the whole job group (bash + in-flight psql) dies
    # together, so no orphaned chunk transaction can commit after the resume
    # run reads the cursor. Either the chunk committed before this point
    # (resume sees the advanced cursor) or its transaction aborts — both are
    # consistent resume states.
    kill -9 -- "-$imp" 2>/dev/null
    kstatus=0
    wait "$imp" 2>/dev/null || kstatus=$?
    if (( kstatus == 0 )); then
      echo "FAIL: importer exited 0 before kill -9 landed; resume untested" >&2
      fail=1
    else
      echo "killed mid-flight at cursor artist=$killed_at release=$rel_at recording=$rec_at track=$trk_at (signal, wait=$kstatus); resuming"
      partial="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track);')"
      if [[ "$partial" == "120|60|60|61" ]]; then
        echo "FAIL: kill left nothing to resume ($partial already fully loaded)" >&2
        fail=1
      else
        bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
        check "resume: artists" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
        check "resume: releases" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
        check "resume: recordings" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
        check "resume: tracks" "61" "$(db -c 'SELECT count(*) FROM mb_track;')"
        check "resume: dlq" "4" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
        check "resume: artist dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_artist GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: release dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_release GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: recording dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_recording GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: artist cursor" "123" "$(cursor artist "$FIX/mbdump-artist.tsv")"
      fi
    fi
  fi
fi

echo "=== 5. duplicate MBID anywhere in one dump is refused ==="
reset_db
DUPE="$FIX-dupe"
rm -rf "$DUPE"
mkdir -p "$DUPE"
cp "$FIX/mbdump-release.tsv" "$FIX/mbdump-recording.tsv" "$FIX/mbdump-track.tsv" "$DUPE/"
python3 - "$DUPE" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    for i in range(1, 31):
        m = eid(1) if i == 30 else eid(i)
        f.write(f'{m}\tBand {i}\t{i}, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
EOF
if MB_IMPORT_CHUNK_LINES=10 bash "$SELF_DIR/import.sh" "$DUPE" >/dev/null 2>&1; then
  echo "FAIL: dupe dump imported without error" >&2
  fail=1
else
  echo "ok: dupe dump refused"
fi
check "dupe: cursor remains at zero" "0" "$(cursor artist "$DUPE/mbdump-artist.tsv")"
check "dupe: no rows loaded" "0" "$(db -c 'SELECT count(*) FROM mb_artist;')"

echo "=== 6. cursor ahead of dump is refused ==="
reset_db
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
db -c "UPDATE mb_import_cursor SET lines_done = 9999 WHERE table_name='artist' AND dump_id='$(dump_id "$FIX/mbdump-artist.tsv")';" >/dev/null
if bash "$SELF_DIR/import.sh" "$FIX" >/dev/null 2>&1; then
  echo "FAIL: shrunken dump imported without error" >&2
  fail=1
else
  echo "ok: shrunken dump refused"
fi
db -c "UPDATE mb_import_cursor SET lines_done = 123 WHERE table_name='artist' AND dump_id='$(dump_id "$FIX/mbdump-artist.tsv")';" >/dev/null

echo "=== 7. DUMP_SEQ seeds forward only ==="
db -c "UPDATE mb_import_state SET last_packet = 0;" >/dev/null
DUMP_SEQ=7 bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
check "seed advances" "7" "$(db -c 'SELECT last_packet FROM mb_import_state WHERE id = 1;')"
DUMP_SEQ=5 bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
check "older seed never rewinds" "7" "$(db -c 'SELECT last_packet FROM mb_import_state WHERE id = 1;')"

echo "=== 8. FK orphans land in DLQ, chunk still commits ==="
reset_db
FKDIR="$FIX-fk"
rm -rf "$FKDIR"
mkdir -p "$FKDIR"
python3 - "$FKDIR" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write('not-a-uuid\tBad\tBad\tGroup\tUS\t2024-05-01T12:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tGood Album\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(424242)}\tOrphan Album\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100003)}\t{eid(1)}\tSecond Album\tEP\t2023\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(100001)}\tGood Track\t1\t180000\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(100002)}\tOrphan Track\t1\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(200001)}\t1\t1\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(200001)}\t1\t1\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(100001)}\t{eid(200002)}\t1\t2\t2024-05-03T00:00:00Z\n')
EOF
# Old behavior: the orphan's REFERENCES violation rolled back the whole
# chunk, the cursor never advanced, and the row was never DLQ'd. Now the
# import must exit 0 with orphans filed and good rows committed.
if ! bash "$SELF_DIR/import.sh" "$FKDIR" >/dev/null 2>&1; then
  echo "FAIL: import aborted on FK orphan rows (poison must never abort)" >&2
  fail=1
else
  echo "ok: import completed with FK orphans present"
fi
check "fk: artists loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "fk: releases loaded" "2" "$(db -c 'SELECT count(*) FROM mb_release;')"
check "fk: recordings loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording;')"
check "fk: tracks loaded" "1" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "fk: artist cursor" "2" "$(cursor artist "$FKDIR/mbdump-artist.tsv")"
check "fk: release cursor" "3" "$(cursor release "$FKDIR/mbdump-release.tsv")"
check "fk: recording cursor" "2" "$(cursor recording "$FKDIR/mbdump-recording.tsv")"
check "fk: track cursor" "3" "$(cursor track "$FKDIR/mbdump-track.tsv")"
check "fk: dlq rows" "5" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "fk: dlq reasons" "bad_uuid,fk_missing_release,fk_missing_artist,fk_missing_release,fk_missing_recording" \
  "$(db -c "SELECT string_agg(reason, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "fk: orphan raw keeps dump key" "$(python3 -c 'import uuid; print(uuid.UUID(int=100002))')" \
  "$(db -c "SELECT split_part(raw_line, chr(9), 1) FROM mb_import_dlq WHERE reason='fk_missing_artist';")"

echo "=== 9. validator write failure exits non-zero ==="
# A short valid.tsv with a 0 exit would advance the cursor past rows that
# never loaded, skipping them forever on rerun. /dev/full makes every
# write fail with ENOSPC.
if [[ -c /dev/full ]]; then
  printf '%s\n' "$(python3 -c 'import uuid; print(f"{uuid.UUID(int=5)}\tBand\tBand\tGroup\tUS\t2024-05-01T12:00:00Z")')" >"$FIX/io.tsv"
  validator_code=0
  "$VALIDATE_BIN" --table artist --in "$FIX/io.tsv" --valid-out /dev/full --poison-out "$FIX/io.poison" --line-offset 0 >/dev/null 2>&1 || validator_code=$?
  check "validator exits 2 on unwritable output" "2" "$validator_code"
else
  echo "ok: /dev/full unavailable on this platform, skipping write-failure check"
fi

echo "=== 10. all-poison dump is refused, replication state untouched ==="
# Regression test for the all-poison refusal guard: with
# refuse_all_poison_dump neutered (`return 0`) both runs below exit 0 and the
# second seeds last_packet=500 over an empty table. Assert non-zero exits on
# the chunk-loop path (first run) and the already-complete path (rerun), with
# no rows loaded and last_packet still 0 either way.
reset_db
POISON="$FIX-poison"
rm -rf "$POISON"
mkdir -p "$POISON"
# Release/recording rows carry \N parents on purpose: they are valid rows
# that do not depend on mb_artist/mb_release. Do NOT copy the FK-linked
# fixtures here — with an empty mb_artist every copied release would be an
# FK orphan, so on the rerun the release table would trip its own
# all-poison guard and refuse for the wrong reason, leaving the artist
# already-complete-path guard (import.sh refuse before "already complete")
# unpinned: deleting that call would leave this suite green while the
# rerun seeds last_packet over an empty mb_artist. With \N parents the
# rerun can only be refused by that guard.
python3 - "$POISON" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    for i in range(5):
        f.write(f'not-a-uuid-{i}\tBad {i}\tBad {i}\tGroup\tUS\tnot-a-date\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(300000+i)}\t\\N\tAlbum {i}\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(400000+i)}\t\\N\tTrack {i}\t1\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(300000+i)}\t{eid(400000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
EOF
if DUMP_SEQ=500 bash "$SELF_DIR/import.sh" "$POISON" >/dev/null 2>&1; then
  echo "FAIL: all-poison dump imported without error" >&2
  fail=1
else
  echo "ok: all-poison dump refused on first run"
fi
check "poison: no rows loaded" "0" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "poison: DLQ holds every row" "5" "$(db -c "SELECT count(*) FROM mb_import_dlq WHERE table_name='artist';")"
check "poison: replication state untouched" "0" "$(db -c 'SELECT last_packet FROM mb_import_state WHERE id = 1;')"
if DUMP_SEQ=500 bash "$SELF_DIR/import.sh" "$POISON" >/dev/null 2>&1; then
  echo "FAIL: all-poison rerun imported without error (already-complete path)" >&2
  fail=1
else
  echo "ok: all-poison rerun refused on already-complete path"
fi
check "poison rerun: still no rows" "0" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "poison rerun: downstream tables never loaded" "0" "$(db -c 'SELECT (SELECT count(*) FROM mb_release) + (SELECT count(*) FROM mb_recording) + (SELECT count(*) FROM mb_track);')"
check "poison rerun: replication state still untouched" "0" "$(db -c 'SELECT last_packet FROM mb_import_state WHERE id = 1;')"

echo "=== 11. changed tracks: repoint replaces, dropped position is pruned ==="
# The mb_track key is the position (release, medium, track), so a newer
# dump that repoints track 1 at a different recording must leave exactly
# one row at that position, and a position the newer dump no longer
# lists must be pruned — never two recordings for one track downstream.
reset_db
CHG="$FIX-changed"
rm -rf "$CHG" "$CHG-newer" "$CHG-old"
mkdir -p "$CHG"
python3 - "$CHG" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$CHG" >/dev/null
check "changed: tracks before" "3" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "changed: track 1 before" "$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')" \
  "$(db -c 'SELECT recording_mbid FROM mb_track WHERE medium_no = 1 AND track_no = 1;')"
cp -R "$CHG" "$CHG-newer"
python3 - "$CHG-newer" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    # Track 1 repointed at recording 3, track 2 unchanged, track 3 dropped.
    f.write(f'{eid(100001)}\t{eid(200003)}\t1\t1\t2025-05-03T00:00:00Z\n')
    f.write(f'{eid(100001)}\t{eid(200002)}\t1\t2\t2025-05-03T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$CHG-newer" >/dev/null
check "changed: tracks after" "2" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "changed: track 1 repointed" "$(python3 -c 'import uuid; print(uuid.UUID(int=200003))')" \
  "$(db -c 'SELECT recording_mbid FROM mb_track WHERE medium_no = 1 AND track_no = 1;')"
check "changed: one row per position" "0" \
  "$(db -c 'SELECT count(*) FROM (SELECT medium_no, track_no FROM mb_track GROUP BY 1, 2 HAVING count(*)>1) d;')"
check "changed: dropped track pruned" "0" "$(db -c 'SELECT count(*) FROM mb_track WHERE track_no = 3;')"
check "changed: album order after repoint" \
  "$(python3 -c 'import uuid; print(str(uuid.UUID(int=200003)) + chr(44) + str(uuid.UUID(int=200002)))')" \
  "$(db -c 'SELECT string_agg(recording_mbid::text, chr(44) ORDER BY medium_no, track_no, recording_mbid) FROM mb_track;')"
# An older dump (new bytes, so it really reloads; older timestamps)
# must neither roll the repoint back nor resurrect the dropped track:
# the forward-only upsert guard and the prune's updated_at comparison
# both refuse to move rows backwards.
cp -R "$CHG" "$CHG-old"
python3 - "$CHG-old" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in (3, 2, 1):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$CHG-old" >/dev/null
check "changed: older dump leaves rows alone" "2" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "changed: older dump keeps repoint" "$(python3 -c 'import uuid; print(uuid.UUID(int=200003))')" \
  "$(db -c 'SELECT recording_mbid FROM mb_track WHERE medium_no = 1 AND track_no = 1;')"

echo "=== 12. interrupted multi-chunk track import never truncates a release ==="
# The prune runs only in the final track chunk's transaction. A release
# whose tracks straddle a chunk boundary in a newer dump must keep its
# older-dump rows for not-yet-reloaded positions when the import stops
# between chunks (the route serves mb_track directly, so a truncated
# listing would be cached and persisted as final). Failure is injected
# deterministically: a validator wrapper that fails on the second
# track chunk, after the first chunk (and its cursor advance) commits.
reset_db
SPLIT="$FIX-split"
SPLITN="$FIX-split-newer"
SPLITTOOLS="$FIX-split-tools"
rm -rf "$SPLIT" "$SPLITN" "$SPLITTOOLS"
mkdir -p "$SPLIT" "$SPLITTOOLS"
python3 - "$SPLIT" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 11):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 11):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$SPLIT" >/dev/null
check "split: tracks before" "10" "$(db -c 'SELECT count(*) FROM mb_track;')"
cp -R "$SPLIT" "$SPLITN"
python3 - "$SPLITN" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    # Track 1 repointed at recording 10, track 10 dropped: 9 rows, so
    # chunk size 4 splits this one release across three chunks.
    f.write(f'{eid(100001)}\t{eid(200010)}\t1\t1\t2025-05-03T00:00:00Z\n')
    for i in range(2, 10):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2025-05-03T00:00:00Z\n')
EOF
python3 - "$SPLITTOOLS/fail-second-track-validate" "$VALIDATE_BIN" "$SPLITTOOLS/track-validate-count" <<'EOF'
import os, sys
path, real, count = sys.argv[1], sys.argv[2], sys.argv[3]
script = f"""#!/usr/bin/env bash
if [[ "${{2:-}}" == "track" ]]; then
  n=0
  [[ -f '{count}' ]] && n="$(cat '{count}')"
  n=$((n + 1))
  printf '%s' "$n" >'{count}'
  if (( n == 2 )); then
    echo "injected validator failure on track chunk 2" >&2
    exit 1
  fi
fi
exec '{real}' "$@"
"""
with open(path, 'w') as f:
    f.write(script)
os.chmod(path, 0o755)
EOF
if MB_IMPORT_CHUNK_LINES=4 MB_VALIDATE_BIN="$SPLITTOOLS/fail-second-track-validate" bash "$SELF_DIR/import.sh" "$SPLITN" >/dev/null 2>&1; then
  echo "FAIL: split dump imported without the injected validator failure" >&2
  fail=1
else
  echo "ok: injected failure stopped the split import between track chunks"
fi
check "split: track cursor stopped after chunk 1" "4" "$(cursor track "$SPLITN/mbdump-track.tsv")"
check "split: interrupted import keeps all 10 old positions" "10" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "split: not-yet-reloaded position 10 still present" "1" "$(db -c 'SELECT count(*) FROM mb_track WHERE track_no = 10;')"
check "split: positions unchanged while interrupted" "1,2,3,4,5,6,7,8,9,10" \
  "$(db -c 'SELECT string_agg(track_no::text, chr(44) ORDER BY track_no) FROM mb_track;')"
MB_IMPORT_CHUNK_LINES=4 bash "$SELF_DIR/import.sh" "$SPLITN" >/dev/null
check "split: resumed import completes cursor" "9" "$(cursor track "$SPLITN/mbdump-track.tsv")"
check "split: final prune drops position 10" "9" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "split: track 1 repointed after resume" "$(python3 -c 'import uuid; print(uuid.UUID(int=200010))')" \
  "$(db -c 'SELECT recording_mbid FROM mb_track WHERE medium_no = 1 AND track_no = 1;')"
check "split: final positions" "1,2,3,4,5,6,7,8,9" \
  "$(db -c 'SELECT string_agg(track_no::text, chr(44) ORDER BY track_no) FROM mb_track;')"

rm -rf "$FIX" "$DUPE" "$NEWER" "$FKDIR" "$POISON" "$CHG" "$CHG-newer" "$CHG-old" "$SPLIT" "$SPLITN" "$SPLITTOOLS"
if (( fail != 0 )); then
  echo "test-import: FAILED" >&2
  exit 1
fi
echo "test-import: all checks passed"
db_show -c "SELECT table_name, dump_id, lines_done FROM mb_import_cursor ORDER BY 1, 2;" \
  -c "SELECT table_name, reason, count(*) FROM mb_import_dlq GROUP BY 1, 2 ORDER BY 1, 2;"
