#!/usr/bin/env bash
# Integration test for import.sh: chunked import, kill-resume, same-dump
# rerun, newer-dump upsert, DLQ query, and refusal guards. Needs psql,
# python3 and mb-validate
# on PATH (or MB_VALIDATE_BIN), plus DATABASE_URL to a SCRATCH database.
# DESTRUCTIVE: truncates mb_artist/mb_release/mb_recording/mb_import_dlq and
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
  db -c "TRUNCATE mb_recording, mb_release, mb_artist, mb_import_dlq;" \
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
EOF

echo "--- target: $(db -c "SELECT current_database() || chr(47) || current_user || '@' || coalesce(inet_server_addr()::text, 'socket');")"

echo "=== 1. full import: counts, cursor, DLQ ==="
reset_db
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
check "artists loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "releases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
check "recordings loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
check "artist cursor" "123" "$(cursor artist "$FIX/mbdump-artist.tsv")"
check "release cursor" "61" "$(cursor release "$FIX/mbdump-release.tsv")"
check "recording cursor" "60" "$(cursor recording "$FIX/mbdump-recording.tsv")"
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
before="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
after="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
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
check "new dump preserves table row counts" "120|60|60"   "$(db -c 'SELECT (SELECT count(*) FROM mb_artist) || chr(124) || (SELECT count(*) FROM mb_release) || chr(124) || (SELECT count(*) FROM mb_recording);')"
check "new dump starts an independent cursor" "123" "$(cursor artist "$NEWER/mbdump-artist.tsv")"
check "DLQ retains poison from both dumps" "7" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "same line poison is keyed by dump" "2" \
  "$(db -c "SELECT count(DISTINCT dump_id) FROM mb_import_dlq WHERE table_name='artist' AND line_no=121;")"

echo "=== 4. kill -9 mid-import resumes from cursor ==="
reset_db
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null 2>&1 &
imp=$!
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
  kill "$imp" 2>/dev/null || true
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
  if [[ "$killed_at" == "123" && "$rel_at" == "61" && "$rec_at" == "60" ]]; then
    echo "FAIL: import completed before kill -9 (nothing to resume)" >&2
    kill "$imp" 2>/dev/null || true
    wait "$imp" 2>/dev/null || true
    fail=1
  else
    kill -9 "$imp" 2>/dev/null
    kstatus=0
    wait "$imp" 2>/dev/null || kstatus=$?
    if (( kstatus == 0 )); then
      echo "FAIL: importer exited 0 before kill -9 landed; resume untested" >&2
      fail=1
    else
      echo "killed mid-flight at cursor artist=$killed_at release=$rel_at recording=$rec_at (signal, wait=$kstatus); resuming"
      partial="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording);')"
      if [[ "$partial" == "120|60|60" ]]; then
        echo "FAIL: kill left nothing to resume ($partial already fully loaded)" >&2
        fail=1
      else
        bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
        check "resume: artists" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
        check "resume: releases" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
        check "resume: recordings" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
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
cp "$FIX/mbdump-release.tsv" "$FIX/mbdump-recording.tsv" "$DUPE/"
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
check "fk: artist cursor" "2" "$(cursor artist "$FKDIR/mbdump-artist.tsv")"
check "fk: release cursor" "3" "$(cursor release "$FKDIR/mbdump-release.tsv")"
check "fk: recording cursor" "2" "$(cursor recording "$FKDIR/mbdump-recording.tsv")"
check "fk: dlq rows" "3" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "fk: dlq reasons" "bad_uuid,fk_missing_release,fk_missing_artist" \
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

rm -rf "$FIX" "$DUPE" "$NEWER" "$FKDIR"
if (( fail != 0 )); then
  echo "test-import: FAILED" >&2
  exit 1
fi
echo "test-import: all checks passed"
db_show -c "SELECT table_name, dump_id, lines_done FROM mb_import_cursor ORDER BY 1, 2;" \
  -c "SELECT table_name, reason, count(*) FROM mb_import_dlq GROUP BY 1, 2 ORDER BY 1, 2;"
