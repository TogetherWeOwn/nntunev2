#!/usr/bin/env bash
# Runs in the importer CI image with no network. Starts its own fixture DB.
set -euo pipefail
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../../.." && pwd -P)"
scratch="$(mktemp -d /tmp/nntune-mb.XXXXXX)"
cleanup() {
  if [[ -f "$scratch/data/postmaster.pid" ]]; then
    pg_ctl -D "$scratch/data" -m fast -w stop >/dev/null
  fi
  rm -r -- "$scratch"
}
trap cleanup EXIT

# All reads and writes use the new cluster's private socket. Strip inherited
# libpq values so neither a service file nor hostaddr can redirect them.
for name in "${!PG@}"; do unset "$name"; done
export PGHOST="$scratch" PGUSER=postgres PGDATABASE=postgres PSQLRC=/dev/null
export DATABASE_URL="host=$scratch user=postgres dbname=postgres"
export MB_IMPORT_CHUNK_LINES=1
unset DUMP_SEQ
initdb -D "$scratch/data" -A trust --no-sync >/dev/null
pg_ctl -D "$scratch/data" -l "$scratch/postgres.log" -o "-c listen_addresses='' -k $scratch" -w start >/dev/null

db() { psql -Xwq -v ON_ERROR_STOP=1 "$@"; }
PGOPTIONS='-c search_path=public,pg_catalog' db \
  -f "$repo/migrations/0001_mb_mirror.sql" -f "$repo/migrations/0014_mb_import_resume_dlq.sql" \
  -f "$repo/migrations/0016_mb_track.sql" -f "$repo/migrations/0017_mb_replication_packet_ledger.sql" \
  -f "$repo/migrations/0030_mb_mirror_w6b.sql"
db <<'SQL'
CREATE ROLE fixture_observer LOGIN;
GRANT SELECT ON ALL TABLES IN SCHEMA public TO fixture_observer;
ALTER ROLE fixture_observer SET log_statement = 'all';
SQL

# Reuse the ETL's synthetic golden projections, swapping out art and URL
# values and reversing file order so it differs from the listing order.
mkdir "$scratch/curated"
python3 -I - "$repo/containers/mb-import/tests/expected" "$scratch" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

goldens, scratch = map(Path, sys.argv[1:])
tables = {}
for table in ("artist", "release_group", "release", "recording", "track", "url",
              "artist_url", "recording_url", "recording_music_video", "recording_isrc"):
    name = f"mbdump-{table}.tsv"
    rows = [line.split("\t") for line in (goldens / name).read_text().splitlines()]
    if table == "release":
        for row in rows:
            row[5] = r"\N"
    if table == "url":
        for number, row in enumerate(rows, 1):
            row[1] = f"https://example.test/mb/{number}"
    if table == "track":
        rows.reverse()
    data = "".join("\t".join(row) + "\n" for row in rows).encode()
    (scratch / "curated" / name).write_bytes(data)
    tables[table] = {"sha256": hashlib.sha256(data).hexdigest(), "lines": len(rows), "rows": len(rows)}
artist = "11111111-1111-4111-8111-111111111111"
recording = "88888888-8888-4888-8888-888888888888"
probes = [
    {"query": "A  RELEASE", "release_mbid": "55555555-5555-4555-8555-555555555555",
     "artist_mbid": artist, "tracks": [[1, 1, recording], [1, 2, recording], [2, 1, recording]]},
    {"query": "Second 'release'", "release_mbid": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
     "artist_mbid": artist, "tracks": [[1, 1, recording]]},
]
evidence = {"tables": tables, "probes": probes}
(scratch / "evidence.json").write_text(json.dumps(evidence))
for name, control in (("tab", "\t"), ("next-line", "\u0085")):
    probes[0]["query"] = f"A{control}release"
    (scratch / f"{name}.json").write_text(json.dumps(evidence))
probes[0]["query"] = "A  RELEASE"
tables["url"]["sha256"] = "0" * 64
tables["recording"]["rows"] = 1
probes[0]["tracks"].reverse()
probes[1]["artist_mbid"] = "22222222-2222-4222-8222-222222222222"
(scratch / "mismatch.json").write_text(json.dumps(evidence))
PY

expect_readiness() {
  local expected="$1" evidence="$2" status=0 output
  shift 2
  output="$(PGUSER=fixture_observer python3 -I "$repo/containers/mb-import/check-readiness.py" \
    "$scratch/$evidence.json" 2>&1)" || status=$?
  printf '%s\n' "$output"
  [[ "$status" == "$expected" ]]
  for line in "$@"; do grep -Fxq -- "$line" <<<"$output"; done
}

echo '=== Empty mirror ==='
expect_readiness 1 evidence 'track: mismatch cursor=None rows=0 dlq=0' 'readiness: FAIL'

# A real first chunk commits before a deterministic validator failure. The
# normal validator still runs for every other chunk, including on resume.
cat > "$scratch/interrupt-validator" <<'SH'
#!/usr/bin/env bash
[[ "$2" != track || "${10}" == 0 ]] || exit 2
exec mb-validate "$@"
SH
chmod +x "$scratch/interrupt-validator"
echo '=== Interrupted import ==='
if MB_VALIDATE_BIN="$scratch/interrupt-validator" bash /job/import.sh "$scratch/curated"; then
  echo 'Expected the importer to stop in the track table' >&2
  exit 1
fi
expect_readiness 1 evidence 'track: mismatch cursor=1 rows=1 dlq=0' \
  'probe 1: search=ok recordings=mismatch' 'readiness: FAIL'

echo '=== Completed import ==='
MB_VALIDATE_BIN=mb-validate bash /job/import.sh "$scratch/curated"
expect_readiness 0 evidence 'track: ok cursor=4 rows=4 dlq=0' \
  'probe 1: search=ok recordings=ok' 'probe 2: search=ok recordings=ok' 'readiness: PASS'
# Trigram search can return the same hit for the original term. Verify that
# PostgreSQL received the normalized "a release" in the fixture's private log.
grep -Fq "EXECUTE release_search(convert_from(decode('612072656c65617365', 'hex'), 'UTF8'));" \
  "$scratch/postgres.log"
echo 'ok: PostgreSQL received the normalized release search term'

echo '=== Wrong evidence against completed import ==='
expect_readiness 1 mismatch 'url: mismatch cursor=None rows=3 dlq=0' \
  'recording: mismatch cursor=2 rows=2 dlq=0' \
  'probe 1: search=ok recordings=mismatch' 'probe 2: search=mismatch recordings=ok' 'readiness: FAIL'

echo '=== Search terms rejected before probing ==='
for evidence in tab next-line; do
  expect_readiness 2 "$evidence" 'readiness: could not read valid import evidence'
done

echo '=== Unexpected dead letter against completed import ==='
db <<'SQL'
INSERT INTO mb_import_dlq (table_name, dump_id, line_no, reason, raw_line)
SELECT table_name, dump_id, 1, 'fixture_only', 'synthetic row'
FROM mb_import_cursor WHERE table_name = 'track';
SQL
expect_readiness 1 evidence 'track: mismatch cursor=4 rows=4 dlq=1' \
  'probe 1: search=ok recordings=ok' 'probe 2: search=ok recordings=ok' 'readiness: FAIL'
