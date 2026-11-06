#!/usr/bin/env bash
# Integration test for import.sh: chunked import, kill-resume, same-dump
# rerun, newer-dump upsert, DLQ query, and refusal guards. Needs psql,
# python3 and mb-validate
# on PATH (or MB_VALIDATE_BIN), plus DATABASE_URL to a SCRATCH database.
# DESTRUCTIVE: truncates mb_artist/mb_release_group/mb_release/mb_recording/
# mb_track/mb_url/mb_artist_url/mb_recording_url/mb_recording_music_video/
# mb_recording_isrc/mb_work/mb_label/mb_artist_recording/mb_artist_release/
# mb_artist_work/mb_recording_work/mb_recording_recording/mb_release_credit/
# mb_artist_alias/mb_release_alias/mb_recording_alias/mb_release_group_alias/
# mb_work_alias/mb_label_alias/mb_release_label/mb_import_dlq and
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
  db -c "TRUNCATE mb_track, mb_recording_isrc, mb_recording_music_video, mb_recording_url, mb_artist_url, mb_recording, mb_release, mb_url, mb_release_group, mb_artist, mb_release_label, mb_label_alias, mb_work_alias, mb_release_group_alias, mb_recording_alias, mb_release_alias, mb_artist_alias, mb_release_credit, mb_recording_recording, mb_recording_work, mb_artist_work, mb_artist_release, mb_artist_recording, mb_label, mb_work, mb_import_dlq;" \
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
N_ART, N_GRP, N_REL, N_REC, N_URL = 120, 30, 60, 60, 10
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    for i in range(1, N_ART + 1):
        f.write(f'{eid(i)}\tBand {i}\t{i}, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write('not-a-uuid\tBad\tBad\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write(f'{eid(9001)}\tBad TS\tBad TS\tGroup\tUS\tnot-a-date\n')
    f.write(f'{eid(9002)}\tShort\n')
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    for i in range(1, N_GRP + 1):
        f.write(f'{eid(400000+i)}\tGroup {i}\t{eid((i % N_ART) + 1)}\tAlbum\t2024-05-02T00:00:00Z\n')
    f.write('not-a-group\tBad\t\\N\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid(100000+i)}\t{eid((i % N_ART) + 1)}\tAlbum {i}\tAlbum\t2024\t\\N\t{eid(400000 + (i % N_GRP) + 1)}\t2024-05-02T00:00:00Z\n')
    f.write('also-bad\t\\N\tBad\tAlbum\t2024\t\\N\t\\N\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(200000+i)}\t{eid(100000 + (i % N_REL) + 1)}\tTrack {i}\t{(i % 12) + 1}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(100000 + (i % N_REL) + 1)}\t{eid(200000+i)}\t1\t{(i % 12) + 1}\t2024-05-03T00:00:00Z\n')
    # One recording also appears on a second release (medium 2): the
    # membership file, unlike the canonical recording row, keeps both.
    f.write(f'{eid(100001)}\t{eid(200001)}\t2\t1\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    for i in range(1, N_URL + 1):
        f.write(f'{eid(500000+i)}\thttps://example.test/u{i}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(599999)}\tBad URL\tnot-a-date\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    for i in range(1, N_ART + 1):
        f.write(f'{eid(i)}\t{eid(500000 + (i % N_URL) + 1)}\tyoutube\t2024-05-02T00:00:00Z\n')
    f.write(f'not-an-artist\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        video = 'true' if i % 2 == 0 else 'false'
        f.write(f'{eid(200000+i)}\t{eid(500000 + (i % N_URL) + 1)}\tfree streaming\t{video}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\tyes\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    for i in range(1, N_REC // 2 + 1):
        f.write(f'{eid(200000+i)}\t{eid(200000 + N_REC // 2 + i)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200001)}\tShort\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        # Lowercase on purpose: the validator canonicalizes to the
        # uppercase mirror form, which the suite asserts below.
        f.write(f'{eid(200000+i)}\tusabc{i:07d}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200001)}\tBOGUS\t2024-05-02T00:00:00Z\n')
N_WRK, N_LBL = 20, 10
with open(f'{d}/mbdump-work.tsv', 'w') as f:
    for i in range(1, N_WRK + 1):
        typ = '\\N' if i % 5 == 0 else 'Song'
        f.write(f'{eid(700000+i)}\tWork {i}\t{typ}\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label.tsv', 'w') as f:
    for i in range(1, N_LBL + 1):
        code, typ = ('\\N', '\\N') if i % 5 == 0 else (str(1000+i), 'Production')
        f.write(f'{eid(800000+i)}\tLabel {i}\t{code}\t{typ}\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_recording.tsv', 'w') as f:
    for i in range(1, N_ART + 1):
        f.write(f'{eid(i)}\t{eid(200000 + (i % N_REC) + 1)}\tcomposer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_release.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid((i % N_ART) + 1)}\t{eid(100000+i)}\tproducer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_work.tsv', 'w') as f:
    for i in range(1, N_WRK + 1):
        f.write(f'{eid(i)}\t{eid(700000 + (i % N_WRK) + 1)}\tlyricist\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_work.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(200000+i)}\t{eid(700000 + (i % N_WRK) + 1)}\tperformance\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_recording.tsv', 'w') as f:
    for i in range(1, N_REC // 2 + 1):
        f.write(f'{eid(200000+i)}\t{eid(200000 + ((i + N_REC // 2 - 1) % N_REC) + 1)}\tcover\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_credit.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid(100000+i)}\t0\t{eid((i % N_ART) + 1)}\tCredit {i}\t & \t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_alias.tsv', 'w') as f:
    for i in range(1, N_ART + 1):
        f.write(f'{eid(i)}\tBand {i} Alt\ten\t{i} Alt, Band\ttrue\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_alias.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid(100000+i)}\tAlbum {i} Alt\ten\t{i} Alt, Album\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_alias.tsv', 'w') as f:
    for i in range(1, N_REC + 1):
        f.write(f'{eid(200000+i)}\tTrack {i} Alt\ten\t{i} Alt, Track\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_group_alias.tsv', 'w') as f:
    for i in range(1, N_GRP + 1):
        f.write(f'{eid(400000+i)}\tGroup {i} Alt\ten\t{i} Alt, Group\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-work_alias.tsv', 'w') as f:
    for i in range(1, N_WRK + 1):
        f.write(f'{eid(700000+i)}\tWork {i} Alt\ten\t{i} Alt, Work\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label_alias.tsv', 'w') as f:
    for i in range(1, N_LBL + 1):
        f.write(f'{eid(800000+i)}\tLabel {i} Alt\ten\t{i} Alt, Label\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_label.tsv', 'w') as f:
    for i in range(1, N_REL + 1):
        f.write(f'{eid(100000+i)}\t{eid(800000 + (i % N_LBL) + 1)}\tCAT-{i:03d}\t2024-05-04T00:00:00Z\n')
EOF

echo "--- target: $(db -c "SELECT current_database() || chr(47) || current_user || '@' || coalesce(inet_server_addr()::text, 'socket');")"

echo "=== 1. full import: counts, cursor, DLQ ==="
reset_db
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
check "artists loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
check "release groups loaded" "30" "$(db -c 'SELECT count(*) FROM mb_release_group;')"
check "releases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
check "releases carry group" "60" "$(db -c 'SELECT count(*) FROM mb_release WHERE release_group_mbid IS NOT NULL;')"
check "recordings loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
check "tracks loaded" "61" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "urls loaded" "10" "$(db -c 'SELECT count(*) FROM mb_url;')"
check "artist urls loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
check "recording urls loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording_url;')"
check "music videos loaded" "30" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
check "isrcs loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording_isrc;')"
check "works loaded" "20" "$(db -c 'SELECT count(*) FROM mb_work;')"
check "labels loaded" "10" "$(db -c 'SELECT count(*) FROM mb_label;')"
check "artist recordings loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "artist releases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_artist_release;')"
check "artist works loaded" "20" "$(db -c 'SELECT count(*) FROM mb_artist_work;')"
check "recording works loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording_work;')"
check "song relations loaded" "30" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
check "release credits loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
check "artist aliases loaded" "120" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
check "release aliases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release_alias;')"
check "recording aliases loaded" "60" "$(db -c 'SELECT count(*) FROM mb_recording_alias;')"
check "release group aliases loaded" "30" "$(db -c 'SELECT count(*) FROM mb_release_group_alias;')"
check "work aliases loaded" "20" "$(db -c 'SELECT count(*) FROM mb_work_alias;')"
check "label aliases loaded" "10" "$(db -c 'SELECT count(*) FROM mb_label_alias;')"
check "release labels loaded" "60" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
check "isrcs stored uppercase" "USABC0000001" "$(db -c "SELECT isrc FROM mb_recording_isrc WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
check "shared recording keeps both memberships" "2" \
  "$(db -c "SELECT count(*) FROM mb_track WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
check "artist cursor" "123" "$(cursor artist "$FIX/mbdump-artist.tsv")"
check "release group cursor" "31" "$(cursor release_group "$FIX/mbdump-release_group.tsv")"
check "release cursor" "61" "$(cursor release "$FIX/mbdump-release.tsv")"
check "recording cursor" "60" "$(cursor recording "$FIX/mbdump-recording.tsv")"
check "track cursor" "61" "$(cursor track "$FIX/mbdump-track.tsv")"
check "url cursor" "11" "$(cursor url "$FIX/mbdump-url.tsv")"
check "artist url cursor" "121" "$(cursor artist_url "$FIX/mbdump-artist_url.tsv")"
check "recording url cursor" "61" "$(cursor recording_url "$FIX/mbdump-recording_url.tsv")"
check "music video cursor" "31" "$(cursor recording_music_video "$FIX/mbdump-recording_music_video.tsv")"
check "isrc cursor" "61" "$(cursor recording_isrc "$FIX/mbdump-recording_isrc.tsv")"
check "work cursor" "20" "$(cursor work "$FIX/mbdump-work.tsv")"
check "label cursor" "10" "$(cursor label "$FIX/mbdump-label.tsv")"
check "artist recording cursor" "120" "$(cursor artist_recording "$FIX/mbdump-artist_recording.tsv")"
check "artist release cursor" "60" "$(cursor artist_release "$FIX/mbdump-artist_release.tsv")"
check "artist work cursor" "20" "$(cursor artist_work "$FIX/mbdump-artist_work.tsv")"
check "recording work cursor" "60" "$(cursor recording_work "$FIX/mbdump-recording_work.tsv")"
check "song relation cursor" "30" "$(cursor recording_recording "$FIX/mbdump-recording_recording.tsv")"
check "release credit cursor" "60" "$(cursor release_credit "$FIX/mbdump-release_credit.tsv")"
check "artist alias cursor" "120" "$(cursor artist_alias "$FIX/mbdump-artist_alias.tsv")"
check "release alias cursor" "60" "$(cursor release_alias "$FIX/mbdump-release_alias.tsv")"
check "recording alias cursor" "60" "$(cursor recording_alias "$FIX/mbdump-recording_alias.tsv")"
check "release group alias cursor" "30" "$(cursor release_group_alias "$FIX/mbdump-release_group_alias.tsv")"
check "work alias cursor" "20" "$(cursor work_alias "$FIX/mbdump-work_alias.tsv")"
check "label alias cursor" "10" "$(cursor label_alias "$FIX/mbdump-label_alias.tsv")"
check "release label cursor" "60" "$(cursor release_label "$FIX/mbdump-release_label.tsv")"
check "dlq rows" "10" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "dlq reasons" "bad_uuid,bad_timestamp,column_count,bad_uuid,bad_isrc,column_count,bad_bool,bad_uuid,bad_uuid,bad_timestamp" \
  "$(db -c "SELECT string_agg(reason, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "dlq line_nos" "121,122,123,121,61,31,61,61,31,11" \
  "$(db -c "SELECT string_agg(line_no::text, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "dlq raw round-trips raw key" "not-a-uuid" \
  "$(db -c "SELECT split_part(raw_line, chr(9), 1) FROM mb_import_dlq WHERE reason='bad_uuid' AND table_name='artist';")"
check "artist dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_artist GROUP BY mbid HAVING count(*)>1) d;')"
check "artist url triples unique" "0" "$(db -c 'SELECT count(*) FROM (SELECT artist_mbid, url_mbid, link_name FROM mb_artist_url GROUP BY 1, 2, 3 HAVING count(*)>1) d;')"
check "isrc pairs unique" "0" "$(db -c 'SELECT count(*) FROM (SELECT recording_mbid, isrc FROM mb_recording_isrc GROUP BY 1, 2 HAVING count(*)>1) d;')"
check "artist line range" "1|120|120" \
  "$(db -c 'SELECT min(line_no) || CHR(124) || max(line_no) || CHR(124) || count(*) FROM mb_artist;')"
check "operator DLQ query works" "1" \
  "$(db -c "SELECT count(*) FROM mb_import_dlq WHERE table_name='artist' AND reason='bad_uuid';")"

echo "=== 2. rerun over same dump: zero dupes, zero movement ==="
before="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release_group), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track), (SELECT count(*) FROM mb_url), (SELECT count(*) FROM mb_artist_url), (SELECT count(*) FROM mb_recording_url), (SELECT count(*) FROM mb_recording_music_video), (SELECT count(*) FROM mb_recording_isrc), (SELECT count(*) FROM mb_work), (SELECT count(*) FROM mb_label), (SELECT count(*) FROM mb_artist_recording), (SELECT count(*) FROM mb_artist_release), (SELECT count(*) FROM mb_artist_work), (SELECT count(*) FROM mb_recording_work), (SELECT count(*) FROM mb_recording_recording), (SELECT count(*) FROM mb_release_credit), (SELECT count(*) FROM mb_artist_alias), (SELECT count(*) FROM mb_release_alias), (SELECT count(*) FROM mb_recording_alias), (SELECT count(*) FROM mb_release_group_alias), (SELECT count(*) FROM mb_work_alias), (SELECT count(*) FROM mb_label_alias), (SELECT count(*) FROM mb_release_label), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
after="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release_group), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track), (SELECT count(*) FROM mb_url), (SELECT count(*) FROM mb_artist_url), (SELECT count(*) FROM mb_recording_url), (SELECT count(*) FROM mb_recording_music_video), (SELECT count(*) FROM mb_recording_isrc), (SELECT count(*) FROM mb_work), (SELECT count(*) FROM mb_label), (SELECT count(*) FROM mb_artist_recording), (SELECT count(*) FROM mb_artist_release), (SELECT count(*) FROM mb_artist_work), (SELECT count(*) FROM mb_recording_work), (SELECT count(*) FROM mb_recording_recording), (SELECT count(*) FROM mb_release_credit), (SELECT count(*) FROM mb_artist_alias), (SELECT count(*) FROM mb_release_alias), (SELECT count(*) FROM mb_recording_alias), (SELECT count(*) FROM mb_release_group_alias), (SELECT count(*) FROM mb_work_alias), (SELECT count(*) FROM mb_label_alias), (SELECT count(*) FROM mb_release_label), (SELECT count(*) FROM mb_import_dlq), (SELECT sum(lines_done) FROM mb_import_cursor);')"
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
check "new dump preserves table row counts" "120|30|60|60|61|10|120|60|30|60|20|10|120|60|20|60|30|60|120|60|60|30|20|10|60"   "$(db -c 'SELECT (SELECT count(*) FROM mb_artist) || chr(124) || (SELECT count(*) FROM mb_release_group) || chr(124) || (SELECT count(*) FROM mb_release) || chr(124) || (SELECT count(*) FROM mb_recording) || chr(124) || (SELECT count(*) FROM mb_track) || chr(124) || (SELECT count(*) FROM mb_url) || chr(124) || (SELECT count(*) FROM mb_artist_url) || chr(124) || (SELECT count(*) FROM mb_recording_url) || chr(124) || (SELECT count(*) FROM mb_recording_music_video) || chr(124) || (SELECT count(*) FROM mb_recording_isrc) || chr(124) || (SELECT count(*) FROM mb_work) || chr(124) || (SELECT count(*) FROM mb_label) || chr(124) || (SELECT count(*) FROM mb_artist_recording) || chr(124) || (SELECT count(*) FROM mb_artist_release) || chr(124) || (SELECT count(*) FROM mb_artist_work) || chr(124) || (SELECT count(*) FROM mb_recording_work) || chr(124) || (SELECT count(*) FROM mb_recording_recording) || chr(124) || (SELECT count(*) FROM mb_release_credit) || chr(124) || (SELECT count(*) FROM mb_artist_alias) || chr(124) || (SELECT count(*) FROM mb_release_alias) || chr(124) || (SELECT count(*) FROM mb_recording_alias) || chr(124) || (SELECT count(*) FROM mb_release_group_alias) || chr(124) || (SELECT count(*) FROM mb_work_alias) || chr(124) || (SELECT count(*) FROM mb_label_alias) || chr(124) || (SELECT count(*) FROM mb_release_label);')"
check "new dump starts an independent cursor" "123" "$(cursor artist "$NEWER/mbdump-artist.tsv")"
check "DLQ retains poison from both dumps" "13" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
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
  rg_at="$(cursor release_group "$FIX/mbdump-release_group.tsv")"
  url_at="$(cursor url "$FIX/mbdump-url.tsv")"
  aurl_at="$(cursor artist_url "$FIX/mbdump-artist_url.tsv")"
  rurl_at="$(cursor recording_url "$FIX/mbdump-recording_url.tsv")"
  mv_at="$(cursor recording_music_video "$FIX/mbdump-recording_music_video.tsv")"
  isrc_at="$(cursor recording_isrc "$FIX/mbdump-recording_isrc.tsv")"
  wrk_at="$(cursor work "$FIX/mbdump-work.tsv")"
  lbl_at="$(cursor label "$FIX/mbdump-label.tsv")"
  ar_at="$(cursor artist_recording "$FIX/mbdump-artist_recording.tsv")"
  arel_at="$(cursor artist_release "$FIX/mbdump-artist_release.tsv")"
  aw_at="$(cursor artist_work "$FIX/mbdump-artist_work.tsv")"
  rw_at="$(cursor recording_work "$FIX/mbdump-recording_work.tsv")"
  rr_at="$(cursor recording_recording "$FIX/mbdump-recording_recording.tsv")"
  rc_at="$(cursor release_credit "$FIX/mbdump-release_credit.tsv")"
  aa_at="$(cursor artist_alias "$FIX/mbdump-artist_alias.tsv")"
  rala_at="$(cursor release_alias "$FIX/mbdump-release_alias.tsv")"
  rcal_at="$(cursor recording_alias "$FIX/mbdump-recording_alias.tsv")"
  rga_at="$(cursor release_group_alias "$FIX/mbdump-release_group_alias.tsv")"
  wa_at="$(cursor work_alias "$FIX/mbdump-work_alias.tsv")"
  la_at="$(cursor label_alias "$FIX/mbdump-label_alias.tsv")"
  rl_at="$(cursor release_label "$FIX/mbdump-release_label.tsv")"
  if [[ "$killed_at" == "123" && "$rel_at" == "61" && "$rec_at" == "60" && "$trk_at" == "61" && "$rg_at" == "31" && "$url_at" == "11" && "$aurl_at" == "121" && "$rurl_at" == "61" && "$mv_at" == "31" && "$isrc_at" == "61" && "$wrk_at" == "20" && "$lbl_at" == "10" && "$ar_at" == "120" && "$arel_at" == "60" && "$aw_at" == "20" && "$rw_at" == "60" && "$rr_at" == "30" && "$rc_at" == "60" && "$aa_at" == "120" && "$rala_at" == "60" && "$rcal_at" == "60" && "$rga_at" == "30" && "$wa_at" == "20" && "$la_at" == "10" && "$rl_at" == "60" ]]; then
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
      partial="$(db -c 'SELECT (SELECT count(*) FROM mb_artist), (SELECT count(*) FROM mb_release), (SELECT count(*) FROM mb_recording), (SELECT count(*) FROM mb_track), (SELECT count(*) FROM mb_release_group), (SELECT count(*) FROM mb_url), (SELECT count(*) FROM mb_artist_url), (SELECT count(*) FROM mb_recording_url), (SELECT count(*) FROM mb_recording_music_video), (SELECT count(*) FROM mb_recording_isrc), (SELECT count(*) FROM mb_work), (SELECT count(*) FROM mb_label), (SELECT count(*) FROM mb_artist_recording), (SELECT count(*) FROM mb_artist_release), (SELECT count(*) FROM mb_artist_work), (SELECT count(*) FROM mb_recording_work), (SELECT count(*) FROM mb_recording_recording), (SELECT count(*) FROM mb_release_credit), (SELECT count(*) FROM mb_artist_alias), (SELECT count(*) FROM mb_release_alias), (SELECT count(*) FROM mb_recording_alias), (SELECT count(*) FROM mb_release_group_alias), (SELECT count(*) FROM mb_work_alias), (SELECT count(*) FROM mb_label_alias), (SELECT count(*) FROM mb_release_label);')"
      if [[ "$partial" == "120|60|60|61|30|10|120|60|30|60|20|10|120|60|20|60|30|60|120|60|60|30|20|10|60" ]]; then
        echo "FAIL: kill left nothing to resume ($partial already fully loaded)" >&2
        fail=1
      else
        bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
        check "resume: artists" "120" "$(db -c 'SELECT count(*) FROM mb_artist;')"
        check "resume: releases" "60" "$(db -c 'SELECT count(*) FROM mb_release;')"
        check "resume: recordings" "60" "$(db -c 'SELECT count(*) FROM mb_recording;')"
        check "resume: tracks" "61" "$(db -c 'SELECT count(*) FROM mb_track;')"
        check "resume: release groups" "30" "$(db -c 'SELECT count(*) FROM mb_release_group;')"
        check "resume: urls" "10" "$(db -c 'SELECT count(*) FROM mb_url;')"
        check "resume: artist urls" "120" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
        check "resume: recording urls" "60" "$(db -c 'SELECT count(*) FROM mb_recording_url;')"
        check "resume: music videos" "30" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
        check "resume: isrcs" "60" "$(db -c 'SELECT count(*) FROM mb_recording_isrc;')"
        check "resume: works" "20" "$(db -c 'SELECT count(*) FROM mb_work;')"
        check "resume: labels" "10" "$(db -c 'SELECT count(*) FROM mb_label;')"
        check "resume: artist recordings" "120" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
        check "resume: artist releases" "60" "$(db -c 'SELECT count(*) FROM mb_artist_release;')"
        check "resume: artist works" "20" "$(db -c 'SELECT count(*) FROM mb_artist_work;')"
        check "resume: recording works" "60" "$(db -c 'SELECT count(*) FROM mb_recording_work;')"
        check "resume: song relations" "30" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
        check "resume: release credits" "60" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
        check "resume: artist aliases" "120" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
        check "resume: release aliases" "60" "$(db -c 'SELECT count(*) FROM mb_release_alias;')"
        check "resume: recording aliases" "60" "$(db -c 'SELECT count(*) FROM mb_recording_alias;')"
        check "resume: release group aliases" "30" "$(db -c 'SELECT count(*) FROM mb_release_group_alias;')"
        check "resume: work aliases" "20" "$(db -c 'SELECT count(*) FROM mb_work_alias;')"
        check "resume: label aliases" "10" "$(db -c 'SELECT count(*) FROM mb_label_alias;')"
        check "resume: release labels" "60" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
        check "resume: dlq" "10" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
        check "resume: artist dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_artist GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: release dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_release GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: recording dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT mbid FROM mb_recording GROUP BY mbid HAVING count(*)>1) d;')"
        check "resume: relation dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT artist_mbid, recording_mbid, link_name FROM mb_artist_recording GROUP BY 1, 2, 3 HAVING count(*)>1) d;')"
        check "resume: credit dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT release_mbid, position FROM mb_release_credit GROUP BY 1, 2 HAVING count(*)>1) d;')"
        check "resume: alias dupes" "0" "$(db -c 'SELECT count(*) FROM (SELECT artist_mbid, name, locale FROM mb_artist_alias GROUP BY 1, 2, 3 HAVING count(*)>1) d;')"
        check "resume: artist cursor" "123" "$(cursor artist "$FIX/mbdump-artist.tsv")"
        check "resume: work cursor" "20" "$(cursor work "$FIX/mbdump-work.tsv")"
        check "resume: release label cursor" "60" "$(cursor release_label "$FIX/mbdump-release_label.tsv")"
      fi
    fi
  fi
fi

echo "=== 5. duplicate MBID anywhere in one dump is refused ==="
reset_db
DUPE="$FIX-dupe"
rm -rf "$DUPE"
mkdir -p "$DUPE"
cp "$FIX/mbdump-release_group.tsv" "$FIX/mbdump-release.tsv" "$FIX/mbdump-recording.tsv" "$FIX/mbdump-track.tsv" "$FIX/mbdump-url.tsv" "$FIX/mbdump-artist_url.tsv" "$FIX/mbdump-recording_url.tsv" "$FIX/mbdump-recording_music_video.tsv" "$FIX/mbdump-recording_isrc.tsv" "$DUPE/"
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
check "dupe: no cursor created" "0" "$(db -c "SELECT count(*) FROM mb_import_cursor WHERE table_name='artist' AND dump_id='$(dump_id "$DUPE/mbdump-artist.tsv")';")"
check "dupe: no rows loaded" "0" "$(db -c 'SELECT count(*) FROM mb_artist;')"

echo "=== 5b. phase-2 duplicate / partial snapshots are refused with all state unchanged ==="
# Full-state fingerprint: every mirror table (base + phase 2), the import
# cursor, the DLQ and the replication state. Row content is hashed, so a
# refused import that mutated, inserted or deleted anything (including a
# cursor row for the refused dump's id) changes the fingerprint. The
# duplicate scan must run before ALL database mutation and cursor inserts,
# so a duplicate in a LATE phase-2 table must not leave earlier tables
# loaded or cursors created.
ALL_MIRROR_TABLES="mb_artist mb_release_group mb_release mb_recording mb_track mb_url mb_artist_url mb_recording_url mb_recording_music_video mb_recording_isrc mb_work mb_label mb_artist_recording mb_artist_release mb_artist_work mb_recording_work mb_recording_recording mb_release_credit mb_artist_alias mb_release_alias mb_recording_alias mb_release_group_alias mb_work_alias mb_label_alias mb_release_label mb_import_cursor mb_import_dlq mb_import_state"
state_fingerprint() {
  local t
  for t in $ALL_MIRROR_TABLES; do
    printf '%s=%s\n' "$t" "$(db -c "SELECT count(*) || ':' || coalesce(md5(string_agg(x::text, E'\\n' ORDER BY x::text)), '') FROM $t x;")"
  done
}
# refuse_unchanged <label> <dump-dir> <stderr-pattern>
refuse_unchanged() {
  local label="$1" dir="$2" pattern="$3" before after rc=0 errf
  errf="$(mktemp)"
  before="$(state_fingerprint)"
  DUMP_SEQ=500 MB_IMPORT_CHUNK_LINES=10 bash "$SELF_DIR/import.sh" "$dir" >/dev/null 2>"$errf" || rc=$?
  after="$(state_fingerprint)"
  if [[ "$rc" == "0" ]]; then
    echo "FAIL: $label imported without error" >&2
    fail=1
  else
    echo "ok: $label refused (exit $rc)"
  fi
  if grep -q -- "$pattern" "$errf"; then
    echo "ok: $label refusal message matches '$pattern'"
  else
    echo "FAIL: $label refusal message missing '$pattern'" >&2
    sed 's/^/  stderr: /' "$errf" >&2
    fail=1
  fi
  rm -f "$errf"
  check "$label: rows, cursors, DLQ and replication state unchanged (incl. phase-2 tables)" "$before" "$after"
}
# Phase-2 duplicate snapshots: the rest of the dump is the valid FIX
# snapshot, so earlier tables would load fully before the duplicate table
# is reached if the scan ran lazily.
make_dupe_p2() { # dir table  (append an exact copy of the first row)
  rm -rf "$1"
  mkdir -p "$1"
  cp "$FIX"/mbdump-*.tsv "$1/"
  head -n 1 "$1/mbdump-$2.tsv" >>"$1/mbdump-$2.tsv"
}
make_dupe_p2 "$FIX-dupe-p2-last" release_label
make_dupe_p2 "$FIX-dupe-p2-alias" artist_alias
# Partial phase-2 snapshots: one phase-2 file missing, first / last in order.
PARTIAL_FIRST="$FIX-partial-first"
PARTIAL_LAST="$FIX-partial-last"
for p in "$PARTIAL_FIRST" "$PARTIAL_LAST"; do
  rm -rf "$p"
  mkdir -p "$p"
  cp "$FIX"/mbdump-*.tsv "$p/"
done
rm "$PARTIAL_FIRST/mbdump-work.tsv"
rm "$PARTIAL_LAST/mbdump-release_label.tsv"

for start in empty loaded; do
  reset_db
  if [[ "$start" == "loaded" ]]; then
    bash "$SELF_DIR/import.sh" "$FIX" >/dev/null
  fi
  refuse_unchanged "dupe phase-2 release_label [$start start]" "$FIX-dupe-p2-last" "duplicate key"
  refuse_unchanged "dupe phase-2 artist_alias [$start start]" "$FIX-dupe-p2-alias" "duplicate key"
  refuse_unchanged "partial snapshot missing work [$start start]" "$PARTIAL_FIRST" "phase-2 tables import as a group"
  refuse_unchanged "partial snapshot missing release_label [$start start]" "$PARTIAL_LAST" "phase-2 tables import as a group"
done
# Base-only (pre-phase-2) duplicate refusal stays covered by section 5
# above; assert it here with the same full-state fingerprint too.
reset_db
refuse_unchanged "dupe base-only artist" "$DUPE" "duplicate key"

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
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGood Group\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(400002)}\tOrphan Group\t{eid(434343)}\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tGood Album\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(424242)}\tOrphan Album\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100003)}\t{eid(1)}\tSecond Album\tEP\t2023\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100004)}\t{eid(1)}\tGroup Orphan Album\tAlbum\t2024\t\\N\t{eid(434343)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(100001)}\tGood Track\t1\t180000\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(100002)}\tOrphan Track\t1\t180000\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(200003)}\t{eid(100003)}\tSecond Track\t1\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(200001)}\t1\t1\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(200001)}\t1\t1\t2024-05-03T00:00:00Z\n')
    f.write(f'{eid(100001)}\t{eid(200002)}\t1\t2\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/good\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(500002)}\thttps://example.test/spare\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(1)}\t{eid(535353)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(545454)}\t{eid(500001)}\tfree streaming\tfalse\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200003)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200001)}\t{eid(565656)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(575757)}\tUSABC2400002\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-work.tsv', 'w') as f:
    f.write(f'{eid(700001)}\tGood Work\tSong\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label.tsv', 'w') as f:
    f.write(f'{eid(800001)}\tGood Label\t1903\tProduction\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_recording.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(200001)}\tcomposer\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(1)}\t{eid(634343)}\tcomposer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_release.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(100001)}\tproducer\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(614141)}\t{eid(100001)}\tproducer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_work.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(700001)}\tlyricist\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(1)}\t{eid(644444)}\tlyricist\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_work.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(700001)}\tperformance\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(634343)}\t{eid(700001)}\tperformance\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_recording.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200003)}\tcover\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(200001)}\t{eid(634343)}\tcover\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_credit.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t0\t{eid(1)}\tCredit 1\t & \t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(624242)}\t0\t{eid(1)}\tOrphan Credit\t & \t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100001)}\t1\t{eid(614141)}\tOrphan Credit\t & \t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_alias.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1 Alt\ten\t1 Alt, Band\ttrue\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(614141)}\tOrphan Alt\ten\tAlt, Orphan\ttrue\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_alias.tsv', 'w') as f:
    f.write(f'{eid(100001)}\tGood Album Alt\ten\tAlt, Good Album\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(624242)}\tOrphan Alt\ten\tAlt, Orphan\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_alias.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tGood Track Alt\ten\tAlt, Good Track\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(634343)}\tOrphan Alt\ten\tAlt, Orphan\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_group_alias.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGood Group Alt\ten\tAlt, Good Group\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(654545)}\tOrphan Alt\ten\tAlt, Orphan\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-work_alias.tsv', 'w') as f:
    f.write(f'{eid(700001)}\tGood Work Alt\ten\tAlt, Good Work\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(644444)}\tOrphan Alt\ten\tAlt, Orphan\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label_alias.tsv', 'w') as f:
    f.write(f'{eid(800001)}\tGood Label Alt\ten\tAlt, Good Label\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(664646)}\tOrphan Alt\ten\tAlt, Orphan\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_label.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(800001)}\tCAT-001\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(624242)}\t{eid(800001)}\tCAT-002\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100001)}\t{eid(664646)}\tCAT-003\t2024-05-04T00:00:00Z\n')
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
check "fk: release groups loaded" "1" "$(db -c 'SELECT count(*) FROM mb_release_group;')"
# Four release rows: 100001 + 100003 load; 100002 is DLQ'd
# (fk_missing_artist) and 100004 is DLQ'd (fk_missing_release_group).
check "fk: releases loaded" "2" "$(db -c 'SELECT count(*) FROM mb_release;')"
check "fk: recordings loaded" "2" "$(db -c 'SELECT count(*) FROM mb_recording;')"
check "fk: tracks loaded" "1" "$(db -c 'SELECT count(*) FROM mb_track;')"
check "fk: urls loaded" "2" "$(db -c 'SELECT count(*) FROM mb_url;')"
check "fk: artist urls loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
check "fk: recording urls loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_url;')"
check "fk: music videos loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
check "fk: isrcs loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_isrc;')"
check "fk: works loaded" "1" "$(db -c 'SELECT count(*) FROM mb_work;')"
check "fk: labels loaded" "1" "$(db -c 'SELECT count(*) FROM mb_label;')"
check "fk: artist recordings loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "fk: artist releases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist_release;')"
check "fk: artist works loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist_work;')"
check "fk: recording works loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_work;')"
check "fk: song relations loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
check "fk: release credits loaded" "1" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
check "fk: artist aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
check "fk: release aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_release_alias;')"
check "fk: recording aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_recording_alias;')"
check "fk: release group aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_release_group_alias;')"
check "fk: work aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_work_alias;')"
check "fk: label aliases loaded" "1" "$(db -c 'SELECT count(*) FROM mb_label_alias;')"
check "fk: release labels loaded" "1" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
check "fk: artist cursor" "2" "$(cursor artist "$FKDIR/mbdump-artist.tsv")"
check "fk: release group cursor" "2" "$(cursor release_group "$FKDIR/mbdump-release_group.tsv")"
check "fk: release cursor" "4" "$(cursor release "$FKDIR/mbdump-release.tsv")"
check "fk: recording cursor" "3" "$(cursor recording "$FKDIR/mbdump-recording.tsv")"
check "fk: track cursor" "3" "$(cursor track "$FKDIR/mbdump-track.tsv")"
check "fk: url cursor" "2" "$(cursor url "$FKDIR/mbdump-url.tsv")"
check "fk: artist url cursor" "2" "$(cursor artist_url "$FKDIR/mbdump-artist_url.tsv")"
check "fk: recording url cursor" "2" "$(cursor recording_url "$FKDIR/mbdump-recording_url.tsv")"
check "fk: music video cursor" "2" "$(cursor recording_music_video "$FKDIR/mbdump-recording_music_video.tsv")"
check "fk: isrc cursor" "2" "$(cursor recording_isrc "$FKDIR/mbdump-recording_isrc.tsv")"
check "fk: work cursor" "1" "$(cursor work "$FKDIR/mbdump-work.tsv")"
check "fk: label cursor" "1" "$(cursor label "$FKDIR/mbdump-label.tsv")"
check "fk: artist recording cursor" "2" "$(cursor artist_recording "$FKDIR/mbdump-artist_recording.tsv")"
check "fk: artist release cursor" "2" "$(cursor artist_release "$FKDIR/mbdump-artist_release.tsv")"
check "fk: artist work cursor" "2" "$(cursor artist_work "$FKDIR/mbdump-artist_work.tsv")"
check "fk: recording work cursor" "2" "$(cursor recording_work "$FKDIR/mbdump-recording_work.tsv")"
check "fk: song relation cursor" "2" "$(cursor recording_recording "$FKDIR/mbdump-recording_recording.tsv")"
check "fk: release credit cursor" "3" "$(cursor release_credit "$FKDIR/mbdump-release_credit.tsv")"
check "fk: artist alias cursor" "2" "$(cursor artist_alias "$FKDIR/mbdump-artist_alias.tsv")"
check "fk: release alias cursor" "2" "$(cursor release_alias "$FKDIR/mbdump-release_alias.tsv")"
check "fk: recording alias cursor" "2" "$(cursor recording_alias "$FKDIR/mbdump-recording_alias.tsv")"
check "fk: release group alias cursor" "2" "$(cursor release_group_alias "$FKDIR/mbdump-release_group_alias.tsv")"
check "fk: work alias cursor" "2" "$(cursor work_alias "$FKDIR/mbdump-work_alias.tsv")"
check "fk: label alias cursor" "2" "$(cursor label_alias "$FKDIR/mbdump-label_alias.tsv")"
check "fk: release label cursor" "3" "$(cursor release_label "$FKDIR/mbdump-release_label.tsv")"
check "fk: dlq rows" "26" "$(db -c 'SELECT count(*) FROM mb_import_dlq;')"
check "fk: dlq reasons" "bad_uuid,fk_missing_artist,fk_missing_recording,fk_missing_artist,fk_missing_url,fk_missing_work,fk_missing_label,fk_missing_release,fk_missing_recording,fk_missing_recording,fk_missing_recording,fk_missing_recording,fk_missing_recording,fk_missing_recording,fk_missing_artist,fk_missing_release_group,fk_missing_release,fk_missing_release,fk_missing_artist,fk_missing_artist,fk_missing_release_group,fk_missing_release,fk_missing_label,fk_missing_release,fk_missing_recording,fk_missing_work" \
  "$(db -c "SELECT string_agg(reason, ',' ORDER BY table_name, line_no) FROM mb_import_dlq;")"
check "fk: orphan raw keeps dump key" "$(python3 -c 'import uuid; print(uuid.UUID(int=100002))')" \
  "$(db -c "SELECT split_part(raw_line, chr(9), 1) FROM mb_import_dlq WHERE reason='fk_missing_artist' AND table_name='release';")"
check "fk: phase-2 orphan raw keeps dump key" "$(python3 -c 'import uuid; print(uuid.UUID(int=624242))')" \
  "$(db -c "SELECT split_part(raw_line, chr(9), 1) FROM mb_import_dlq WHERE reason='fk_missing_release' AND table_name='release_credit';")"

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
# The dump is refused at the artist table (all poison) before any
# downstream table runs, so these files only satisfy admission: they are
# plausible valid rows a run that got this far would load.
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'{eid(600000+i)}\tGroup {i}\t\\N\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'{eid(700000+i)}\thttps://example.test/p{i}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'\\N\t{eid(700000+i)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'\\N\t{eid(700000+i)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'{eid(800000+i)}\t{eid(800000+i+10)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    for i in range(1, 3):
        f.write(f'{eid(800000+i)}\tUSABC240{i:04d}\t2024-05-02T00:00:00Z\n')
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
check "poison rerun: downstream tables never loaded" "0" "$(db -c 'SELECT (SELECT count(*) FROM mb_release_group) + (SELECT count(*) FROM mb_release) + (SELECT count(*) FROM mb_recording) + (SELECT count(*) FROM mb_track) + (SELECT count(*) FROM mb_url) + (SELECT count(*) FROM mb_artist_url) + (SELECT count(*) FROM mb_recording_url) + (SELECT count(*) FROM mb_recording_music_video) + (SELECT count(*) FROM mb_recording_isrc) + (SELECT count(*) FROM mb_work) + (SELECT count(*) FROM mb_label) + (SELECT count(*) FROM mb_artist_recording) + (SELECT count(*) FROM mb_artist_release) + (SELECT count(*) FROM mb_artist_work) + (SELECT count(*) FROM mb_recording_work) + (SELECT count(*) FROM mb_recording_recording) + (SELECT count(*) FROM mb_release_credit) + (SELECT count(*) FROM mb_artist_alias) + (SELECT count(*) FROM mb_release_alias) + (SELECT count(*) FROM mb_recording_alias) + (SELECT count(*) FROM mb_release_group_alias) + (SELECT count(*) FROM mb_work_alias) + (SELECT count(*) FROM mb_label_alias) + (SELECT count(*) FROM mb_release_label);')"
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
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/good\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200002)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2024-05-02T00:00:00Z\n')
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
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 11):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 11):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/good\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200002)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2024-05-02T00:00:00Z\n')
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

echo "=== 13. dropped relationships are pruned; direct video URLs resolve ==="
# The four relationship tables prune per-parent newest-dump rows in the
# final chunk (same pattern as the track prune in 11/12): a newer dump
# that drops a link, pair or ISRC must stop serving it. Recording 200003
# carries a direct video-marked URL but no music-video pair, so it also
# proves the read join's direct leg (workers/mb RECORDING_VIDEOS_SQL):
# the query below mirrors that statement's union of direct video-marked
# recording URLs and pair-reached video URLs.
reset_db
REL="$FIX-rel"
rm -rf "$REL" "$REL-newer" "$REL-old"
mkdir -p "$REL"
python3 - "$REL" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/vid\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(500002)}\thttps://example.test/stale\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(1)}\t{eid(500002)}\tfree streaming\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200003)}\t{eid(500002)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200002)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200001)}\tUSABC2400002\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(200002)}\tGBXYZ9900012\t2024-05-02T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$REL" >/dev/null
check "rel: artist urls before" "2" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
check "rel: recording urls before" "3" "$(db -c 'SELECT count(*) FROM mb_recording_url;')"
check "rel: music videos before" "1" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
check "rel: isrcs before" "3" "$(db -c 'SELECT count(*) FROM mb_recording_isrc;')"
cp -R "$REL" "$REL-newer"
python3 - "$REL-newer" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
# Artist drops its free-streaming link; recording 200002 drops its only
# URL row; pair (200001, 200002) is replaced by (200002, 200003), which
# prunes the old pair through endpoint 200002; recording 200001 drops
# its second ISRC. A third URL arrives for 200002's new direct video row.
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/vid\t2025-05-02T00:00:00Z\n')
    f.write(f'{eid(500002)}\thttps://example.test/stale\t2025-05-02T00:00:00Z\n')
    f.write(f'{eid(500003)}\thttps://example.test/vid2\t2025-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2025-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2025-05-02T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(500003)}\tfree streaming\ttrue\t2025-05-02T00:00:00Z\n')
    f.write(f'{eid(200003)}\t{eid(500002)}\tfree streaming\ttrue\t2025-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200002)}\t{eid(200003)}\t2025-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2025-05-02T00:00:00Z\n')
    f.write(f'{eid(200002)}\tGBXYZ9900012\t2025-05-02T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$REL-newer" >/dev/null
check "rel: dropped artist link pruned" "1" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
check "rel: surviving artist link kept" "youtube" "$(db -c "SELECT link_name FROM mb_artist_url WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=1))')';")"
check "rel: dropped recording url pruned" "0" "$(db -c "SELECT count(*) FROM mb_recording_url WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')' AND url_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=500001))')';")"
check "rel: superseded pair pruned" "0" "$(db -c "SELECT count(*) FROM mb_recording_music_video WHERE recording_a_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
check "rel: replacement pair kept" "1" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
check "rel: dropped isrc pruned" "1" "$(db -c "SELECT count(*) FROM mb_recording_isrc WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
check "rel: surviving isrc kept" "USABC2400001" "$(db -c "SELECT isrc FROM mb_recording_isrc WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')';")"
# The videos read union mirrors the production RECORDING_VIDEOS_SQL in
# workers/mb/src/lib.rs (direct video-marked URLs plus pair-reached
# video URLs; the worker's w6b_sql pin asserts the UNION and both legs
# so a reverted direct leg fails that suite): recording 200001 keeps
# only its direct video, 200002 resolves both its direct video and
# 200003's through the new pair, and 200003 (never in a pair with a
# video-marked other side in the old dump) resolves its direct video.
videos_for() {
  db -c "SELECT recording_mbid::text || '|' || url || '|' || link_name FROM (
    SELECT ru.recording_mbid AS recording_mbid, u.url AS url, ru.link_name AS link_name
    FROM mb_recording_url ru JOIN mb_url u ON u.mbid = ru.url_mbid
    WHERE ru.recording_mbid = '$1' AND ru.is_video
    UNION
    SELECT CASE WHEN mv.recording_a_mbid = '$1' THEN mv.recording_b_mbid ELSE mv.recording_a_mbid END, u.url, ru.link_name
    FROM mb_recording_music_video mv
    JOIN mb_recording_url ru ON ru.recording_mbid = CASE WHEN mv.recording_a_mbid = '$1' THEN mv.recording_b_mbid ELSE mv.recording_a_mbid END
    JOIN mb_url u ON u.mbid = ru.url_mbid
    WHERE (mv.recording_a_mbid = '$1' OR mv.recording_b_mbid = '$1') AND ru.is_video) AS videos
  ORDER BY 1;"
}
R1="$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')"
R2="$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')"
R3="$(python3 -c 'import uuid; print(uuid.UUID(int=200003))')"
U1="https://example.test/vid"
U2="https://example.test/stale"
U3="https://example.test/vid2"
check "rel: 200001 videos (direct only)" "$R1|$U1|free streaming" "$(videos_for "$R1")"
check "rel: 200002 videos (direct plus pair)" "$R2|$U3|free streaming,$R3|$U2|free streaming" "$(videos_for "$R2" | paste -sd ',' -)"
check "rel: 200003 videos (direct plus pair)" "$R2|$U3|free streaming,$R3|$U2|free streaming" "$(videos_for "$R3" | paste -sd ',' -)"
# An older dump (new bytes, older timestamps) must neither roll rows back
# nor resurrect pruned relationships: the forward-only upsert guard and
# the per-parent prunes both refuse to move backwards.
cp -R "$REL" "$REL-old"
python3 - "$REL-old" <<'EOF'
import sys
from pathlib import Path
d = Path(sys.argv[1])
for name in ("mbdump-artist_url.tsv", "mbdump-recording_url.tsv",
             "mbdump-recording_music_video.tsv", "mbdump-recording_isrc.tsv"):
    p = d / name
    p.write_text(p.read_text().replace("2024-05-02T00:00:00Z", "2024-06-02T00:00:00Z"))
EOF
bash "$SELF_DIR/import.sh" "$REL-old" >/dev/null
check "rel: older dump leaves artist urls alone" "1" "$(db -c 'SELECT count(*) FROM mb_artist_url;')"
check "rel: older dump leaves recording urls alone" "3" "$(db -c 'SELECT count(*) FROM mb_recording_url;')"
check "rel: older dump leaves pairs alone" "1" "$(db -c 'SELECT count(*) FROM mb_recording_music_video;')"
check "rel: older dump leaves isrcs alone" "2" "$(db -c 'SELECT count(*) FROM mb_recording_isrc;')"

echo "=== 14. dropped phase-2 rows are pruned; older dump moves nothing back ==="
# Same per-parent newest-dump prune pattern as the relationship tables in
# 13, extended to the phase-2 relations, credits, aliases and
# release-label edges: a newer dump that drops a song-relation link, a
# song-relation pair, a credit position, an alias or a label edge must
# stop serving it, and an older dump (new bytes, older timestamps) must
# neither roll rows back nor resurrect pruned rows.
reset_db
REL2="$FIX-rel2"
rm -rf "$REL2" "$REL2-newer" "$REL2-old"
mkdir -p "$REL2"
python3 - "$REL2" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
with open(f'{d}/mbdump-artist.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
    f.write(f'{eid(2)}\tBand 2\t2, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n')
with open(f'{d}/mbdump-release_group.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(400002)}\tGroup 2\t{eid(1)}\tAlbum\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-release.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(1)}\tAlbum\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(2)}\tAlbum 2\tAlbum\t2024\t\\N\t{eid(400001)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(200000+i)}\t{eid(100001)}\tTrack {i}\t{i}\t180000\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-track.tsv', 'w') as f:
    for i in range(1, 4):
        f.write(f'{eid(100001)}\t{eid(200000+i)}\t1\t{i}\t2024-05-03T00:00:00Z\n')
with open(f'{d}/mbdump-url.tsv', 'w') as f:
    f.write(f'{eid(500001)}\thttps://example.test/good\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-artist_url.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(500001)}\tyoutube\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_url.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(500001)}\tfree streaming\ttrue\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_music_video.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200002)}\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-recording_isrc.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tUSABC2400001\t2024-05-02T00:00:00Z\n')
with open(f'{d}/mbdump-work.tsv', 'w') as f:
    f.write(f'{eid(700001)}\tWork 1\tSong\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(700002)}\tWork 2\tSong\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label.tsv', 'w') as f:
    f.write(f'{eid(800001)}\tLabel 1\t1903\tProduction\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(800002)}\tLabel 2\t1904\tProduction\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_recording.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(200001)}\tcomposer\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(1)}\t{eid(200002)}\tproducer\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(2)}\t{eid(200003)}\tperformer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_release.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(100001)}\tproducer\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(2)}\t{eid(100002)}\tproducer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_work.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(700001)}\tlyricist\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(2)}\t{eid(700001)}\tcomposer\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_work.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(700001)}\tperformance\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(700001)}\tarrangement\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_recording.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200002)}\tcover\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(200001)}\t{eid(200003)}\tcover\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(200002)}\t{eid(200003)}\tmedley\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_credit.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t0\t{eid(1)}\tCredit 1\t & \t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100001)}\t1\t{eid(1)}\tCredit 2\t & \t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100002)}\t0\t{eid(2)}\tCredit 9\t & \t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_alias.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1 Alt\ten\t1 Alt, Band\ttrue\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(1)}\tB1\ten\tB1\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(2)}\tBand 2 Alt\ten\tAlt, Band 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_alias.tsv', 'w') as f:
    f.write(f'{eid(100001)}\tAlbum Alt\ten\tAlt, Album\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100002)}\tAlbum 2 Alt\ten\tAlt, Album 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_alias.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tTrack 1 Alt\ten\tAlt, Track 1\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(200002)}\tTrack 2 Alt\ten\tAlt, Track 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_group_alias.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1 Alt\ten\tAlt, Group 1\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(400002)}\tGroup 2 Alt\ten\tAlt, Group 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-work_alias.tsv', 'w') as f:
    f.write(f'{eid(700001)}\tWork 1 Alt\ten\tAlt, Work 1\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(700002)}\tWork 2 Alt\ten\tAlt, Work 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label_alias.tsv', 'w') as f:
    f.write(f'{eid(800001)}\tLabel 1 Alt\ten\tAlt, Label 1\tfalse\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(800002)}\tLabel 2 Alt\ten\tAlt, Label 2\tfalse\t2024-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_label.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(800001)}\tCAT-001\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100001)}\t{eid(800002)}\tCAT-002\t2024-05-04T00:00:00Z\n')
    f.write(f'{eid(100002)}\t{eid(800001)}\tCAT-009\t2024-05-04T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$REL2" >/dev/null
check "rel2: artist recordings before" "3" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "rel2: song relations before" "3" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
check "rel2: release credits before" "3" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
check "rel2: artist aliases before" "3" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
check "rel2: release labels before" "3" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
cp -R "$REL2" "$REL2-newer"
python3 - "$REL2-newer" <<'EOF'
import sys, uuid
d = sys.argv[1]
def eid(n): return str(uuid.UUID(int=n))
# Full newer snapshot: the producer link, the (200001, 200002) pair,
# credit position 1, the B1 alias and the CAT-002 edge are dropped, and one
# parent per phase-2 table loses every edge (artist 2, release 100002,
# recording 200002, group 400002, work 700002, label 800002 and the
# (200002, 200003) pair whose both endpoints vanish). Every surviving row
# carries a newer timestamp so the snapshot prunes fire.
with open(f'{d}/mbdump-artist_recording.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(200001)}\tcomposer\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_release.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(100001)}\tproducer\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_work.tsv', 'w') as f:
    f.write(f'{eid(1)}\t{eid(700001)}\tlyricist\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_work.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(700001)}\tperformance\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_recording.tsv', 'w') as f:
    f.write(f'{eid(200001)}\t{eid(200003)}\tcover\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_credit.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t0\t{eid(1)}\tCredit 1\t & \t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-artist_alias.tsv', 'w') as f:
    f.write(f'{eid(1)}\tBand 1 Alt\ten\t1 Alt, Band\ttrue\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_alias.tsv', 'w') as f:
    f.write(f'{eid(100001)}\tAlbum Alt\ten\tAlt, Album\tfalse\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-recording_alias.tsv', 'w') as f:
    f.write(f'{eid(200001)}\tTrack 1 Alt\ten\tAlt, Track 1\tfalse\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_group_alias.tsv', 'w') as f:
    f.write(f'{eid(400001)}\tGroup 1 Alt\ten\tAlt, Group 1\tfalse\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-work_alias.tsv', 'w') as f:
    f.write(f'{eid(700001)}\tWork 1 Alt\ten\tAlt, Work 1\tfalse\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-label_alias.tsv', 'w') as f:
    f.write(f'{eid(800001)}\tLabel 1 Alt\ten\tAlt, Label 1\tfalse\t2025-05-04T00:00:00Z\n')
with open(f'{d}/mbdump-release_label.tsv', 'w') as f:
    f.write(f'{eid(100001)}\t{eid(800001)}\tCAT-001\t2025-05-04T00:00:00Z\n')
EOF
bash "$SELF_DIR/import.sh" "$REL2-newer" >/dev/null
check "rel2: dropped relation link pruned" "1" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "rel2: surviving relation link kept" "composer" "$(db -c "SELECT link_name FROM mb_artist_recording WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=1))')';")"
check "rel2: dropped pair pruned" "0" "$(db -c "SELECT count(*) FROM mb_recording_recording WHERE recording_a_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200001))')' AND recording_b_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')';")"
check "rel2: surviving pair kept" "1" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
check "rel2: dropped credit position pruned" "1" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
check "rel2: surviving credit position kept" "0" "$(db -c "SELECT position FROM mb_release_credit WHERE release_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=100001))')';")"
check "rel2: dropped alias pruned" "1" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
check "rel2: surviving alias kept" "Band 1 Alt" "$(db -c "SELECT name FROM mb_artist_alias WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=1))')';")"
check "rel2: dropped label edge pruned" "1" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
check "rel2: surviving label edge kept" "CAT-001" "$(db -c "SELECT catalog_number FROM mb_release_label WHERE release_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=100001))')';")"
# A parent the newer snapshot drops entirely must lose its old rows too:
# the snapshot prune keys on the table max, not on the parent's own rows.
check "rel2: vanished artist loses relation links" "0" "$(db -c "SELECT count(*) FROM mb_artist_recording WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=2))')';")"
check "rel2: surviving artist release link kept" "1" "$(db -c 'SELECT count(*) FROM mb_artist_release;')"
check "rel2: vanished artist loses release links" "0" "$(db -c "SELECT count(*) FROM mb_artist_release WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=2))')';")"
check "rel2: surviving artist work link kept" "1" "$(db -c 'SELECT count(*) FROM mb_artist_work;')"
check "rel2: vanished artist loses work links" "0" "$(db -c "SELECT count(*) FROM mb_artist_work WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=2))')';")"
check "rel2: surviving recording work link kept" "1" "$(db -c 'SELECT count(*) FROM mb_recording_work;')"
check "rel2: vanished recording loses work links" "0" "$(db -c "SELECT count(*) FROM mb_recording_work WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')';")"
check "rel2: pair with both endpoints vanished pruned" "0" "$(db -c "SELECT count(*) FROM mb_recording_recording WHERE recording_a_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')' AND recording_b_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200003))')';")"
check "rel2: vanished release loses credits" "0" "$(db -c "SELECT count(*) FROM mb_release_credit WHERE release_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=100002))')';")"
check "rel2: vanished artist loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_artist_alias WHERE artist_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=2))')';")"
check "rel2: surviving release alias kept" "1" "$(db -c 'SELECT count(*) FROM mb_release_alias;')"
check "rel2: vanished release loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_release_alias WHERE release_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=100002))')';")"
check "rel2: surviving recording alias kept" "1" "$(db -c 'SELECT count(*) FROM mb_recording_alias;')"
check "rel2: vanished recording loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_recording_alias WHERE recording_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=200002))')';")"
check "rel2: surviving group alias kept" "1" "$(db -c 'SELECT count(*) FROM mb_release_group_alias;')"
check "rel2: vanished group loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_release_group_alias WHERE release_group_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=400002))')';")"
check "rel2: surviving work alias kept" "1" "$(db -c 'SELECT count(*) FROM mb_work_alias;')"
check "rel2: vanished work loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_work_alias WHERE work_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=700002))')';")"
check "rel2: surviving label alias kept" "1" "$(db -c 'SELECT count(*) FROM mb_label_alias;')"
check "rel2: vanished label loses aliases" "0" "$(db -c "SELECT count(*) FROM mb_label_alias WHERE label_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=800002))')';")"
check "rel2: vanished release loses label edges" "0" "$(db -c "SELECT count(*) FROM mb_release_label WHERE release_mbid='$(python3 -c 'import uuid; print(uuid.UUID(int=100002))')';")"
# An older dump (new bytes, older timestamps) must neither roll rows back
# nor resurrect pruned rows: the forward-only upsert guard and the
# snapshot prunes both refuse to move backwards.
cp -R "$REL2" "$REL2-old"
python3 - "$REL2-old" <<'EOF'
import sys
from pathlib import Path
d = Path(sys.argv[1])
for name in ("mbdump-artist_recording.tsv", "mbdump-recording_recording.tsv",
             "mbdump-release_credit.tsv", "mbdump-artist_alias.tsv",
             "mbdump-release_label.tsv"):
    p = d / name
    p.write_text(p.read_text().replace("2024-05-04T00:00:00Z", "2024-06-04T00:00:00Z"))
EOF
bash "$SELF_DIR/import.sh" "$REL2-old" >/dev/null
check "rel2: older dump leaves relation links alone" "1" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "rel2: older dump leaves pairs alone" "1" "$(db -c 'SELECT count(*) FROM mb_recording_recording;')"
check "rel2: older dump leaves credits alone" "1" "$(db -c 'SELECT count(*) FROM mb_release_credit;')"
check "rel2: older dump leaves aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_artist_alias;')"
check "rel2: older dump leaves label edges alone" "1" "$(db -c 'SELECT count(*) FROM mb_release_label;')"
check "rel2: older dump leaves release links alone" "1" "$(db -c 'SELECT count(*) FROM mb_artist_release;')"
check "rel2: older dump leaves work links alone" "1" "$(db -c 'SELECT count(*) FROM mb_artist_work;')"
check "rel2: older dump leaves recording work links alone" "1" "$(db -c 'SELECT count(*) FROM mb_recording_work;')"
check "rel2: older dump leaves release aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_release_alias;')"
check "rel2: older dump leaves recording aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_recording_alias;')"
check "rel2: older dump leaves group aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_release_group_alias;')"
check "rel2: older dump leaves work aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_work_alias;')"
check "rel2: older dump leaves label aliases alone" "1" "$(db -c 'SELECT count(*) FROM mb_label_alias;')"

echo "=== 15. all-DLQ phase-2 dumps never prune existing rows, including on resume ==="
# Mixed-age rows can remain after an interrupted older import. An incoming
# dump with no loadable rows must not prune them using the stored table max.
# Reconstruct a committed first chunk to cover resume without timing races.
DLQ2="$FIX-dlq2"
rm -rf "$DLQ2"
cp -R "$REL2" "$DLQ2"
relation_state() {
  db -c 'SELECT row_to_json(r) FROM mb_artist_recording r ORDER BY artist_mbid, recording_mbid, link_name;'
}
for reason in fk_missing_artist bad_uuid; do
  python3 - "$DLQ2/mbdump-artist_recording.tsv" "$reason" <<'EOF'
import sys, uuid
path, reason = sys.argv[1:]
parent = str(uuid.UUID(int=999999)) if reason == "fk_missing_artist" else "not-a-uuid"
with open(path, 'w') as f:
    for i in (1, 2):
        f.write(f'{parent}\t{uuid.UUID(int=200000+i)}\tcomposer\t2026-05-04T00:00:00Z\n')
EOF
  for completed in 0 1; do
    reset_db
    bash "$SELF_DIR/import.sh" "$REL2" >/dev/null
    db -c "UPDATE mb_artist_recording SET updated_at = '2023-05-04T00:00:00Z' WHERE link_name = 'producer';" >/dev/null
    before="$(relation_state)"
    hash="$(dump_id "$DLQ2/mbdump-artist_recording.tsv")"
    if (( completed == 1 )); then
      db -c "INSERT INTO mb_import_cursor (table_name, dump_id, lines_done) VALUES ('artist_recording', '$hash', 1) ON CONFLICT (table_name, dump_id) DO UPDATE SET lines_done = 1;" \
         -c "INSERT INTO mb_import_dlq (table_name, dump_id, line_no, reason, raw_line) VALUES ('artist_recording', '$hash', 1, '$reason', 'seeded first poison chunk');" >/dev/null
    fi
    for attempt in first rerun; do
      if DUMP_SEQ=99 MB_IMPORT_CHUNK_LINES=1 bash "$SELF_DIR/import.sh" "$DLQ2" >"$DLQ2/run.log" 2>&1; then
        echo "FAIL: all-DLQ phase-2 dump imported ($reason, offset $completed, $attempt)" >&2
        fail=1
      fi
      if ! grep -Fq 'refusing artist_recording dump with no valid rows' "$DLQ2/run.log"; then
        echo "FAIL: missing all-DLQ refusal ($reason, offset $completed, $attempt)" >&2
        fail=1
      fi
      check "dlq2: existing rows unchanged ($reason, offset $completed, $attempt)" "$before" "$(relation_state)"
      check "dlq2: cursor complete ($reason, offset $completed, $attempt)" "2" "$(cursor artist_recording "$DLQ2/mbdump-artist_recording.tsv")"
      check "dlq2: both poison rows retained ($reason, offset $completed, $attempt)" "2" "$(db -c "SELECT count(*) FROM mb_import_dlq WHERE table_name = 'artist_recording' AND dump_id = '$hash' AND reason = '$reason';")"
      check "dlq2: refused dump never seeds replication ($reason, offset $completed, $attempt)" "0" "$(db -c 'SELECT last_packet FROM mb_import_state WHERE id = 1;')"
    done
  done
done
# Negative control: a valid row in a prior chunk still authorizes pruning,
# even when the last chunk contains only FK orphans and the run was resumed.
reset_db
bash "$SELF_DIR/import.sh" "$REL2" >/dev/null
python3 - "$DLQ2/mbdump-artist_recording.tsv" <<'EOF'
import sys, uuid
with open(sys.argv[1], 'w') as f:
    f.write(f'{uuid.UUID(int=1)}\t{uuid.UUID(int=200001)}\tcomposer\t2026-05-04T00:00:00Z\n')
    f.write(f'{uuid.UUID(int=999999)}\t{uuid.UUID(int=200002)}\tcomposer\t2026-05-04T00:00:00Z\n')
EOF
hash="$(dump_id "$DLQ2/mbdump-artist_recording.tsv")"
db -c "UPDATE mb_artist_recording SET updated_at = '2026-05-04T00:00:00Z' WHERE link_name = 'composer';" \
   -c "INSERT INTO mb_import_cursor (table_name, dump_id, lines_done) VALUES ('artist_recording', '$hash', 1) ON CONFLICT (table_name, dump_id) DO UPDATE SET lines_done = 1;" >/dev/null
MB_IMPORT_CHUNK_LINES=1 bash "$SELF_DIR/import.sh" "$DLQ2" >/dev/null
check "dlq2: resumed mixed dump still prunes old relations" "1" "$(db -c 'SELECT count(*) FROM mb_artist_recording;')"
check "dlq2: resumed mixed dump retains the valid relation" "composer" "$(db -c 'SELECT link_name FROM mb_artist_recording;')"
check "dlq2: resumed mixed dump completes cursor" "2" "$(cursor artist_recording "$DLQ2/mbdump-artist_recording.tsv")"

rm -rf "$FIX" "$DUPE" "$FIX-dupe-p2-last" "$FIX-dupe-p2-alias" "$PARTIAL_FIRST" "$PARTIAL_LAST" "$NEWER" "$FKDIR" "$POISON" "$CHG" "$CHG-newer" "$CHG-old" "$SPLIT" "$SPLITN" "$SPLITTOOLS" "$REL" "$REL-newer" "$REL-old" "$REL2" "$REL2-newer" "$REL2-old" "$DLQ2"
if (( fail != 0 )); then
  echo "test-import: FAILED" >&2
  exit 1
fi
echo "test-import: all checks passed"
db_show -c "SELECT table_name, dump_id, lines_done FROM mb_import_cursor ORDER BY 1, 2;" \
  -c "SELECT table_name, reason, count(*) FROM mb_import_dlq GROUP BY 1, 2 ORDER BY 1, 2;"
