#!/usr/bin/env bash
# R2 snapshot spike: dump -> curated TSVs -> snapshot, inside the measured
# container (4 vCPU, 12 GiB, /work = a 20 GB filesystem; see measure.sh).
#
# Env: MB_DUMP (fullexport directory, e.g. 20261104-002222).
# Mounts: /src (repo, read-only), /tools (snapshot_build binary), /work.
# Only the CC0 core archive mbdump.tar.bz2 is downloaded; it streams through
# lbzip2 into stream_extract.py, so the compressed and raw dump never land
# on disk. The SHA-256 is checked against the dump's SHA256SUMS before any
# output is used.
set -euo pipefail

: "${MB_DUMP:?MB_DUMP is required}"
case "$MB_DUMP" in
  [0-9][0-9][0-9][0-9][0-9][0-9][0-9][0-9]-[0-9][0-9][0-9][0-9][0-9][0-9]) ;;
  *) echo "build: MB_DUMP must look like YYYYMMDD-HHMMSS" >&2; exit 2 ;;
esac
base="https://data.metabrainz.org/pub/musicbrainz/data/fullexport/${MB_DUMP}"
work=/work
mkdir -p "$work/dump" "$work/tmp" "$work/snap"
export TMPDIR="$work/tmp" SQLITE_TMPDIR="$work/tmp"

phase() { printf '%s\t%s\n' "$1" "$(date -u +%s)" >> "$work/phases.tsv"; }
phase start

curl -fsS --retry 3 "$base/SHA256SUMS" -o "$work/SHA256SUMS"
expected=$(awk '$2 == "mbdump.tar.bz2" || $2 == "*mbdump.tar.bz2" { print $1 }' "$work/SHA256SUMS")
[ -n "$expected" ] || { echo "build: no mbdump.tar.bz2 entry in SHA256SUMS" >&2; exit 1; }

fifo="$work/tmp/download.fifo"
mkfifo "$fifo"
sha256sum < "$fifo" | cut -d' ' -f1 > "$work/dump.sha256" &
sha_pid=$!
curl -fsSL --retry 3 "$base/mbdump.tar.bz2" \
  | tee "$fifo" \
  | lbzip2 -dc -n 4 \
  | python3 /src/containers/mb-import/stream_extract.py "$work/dump"
wait "$sha_pid"
rm -f "$fifo"
if [ "$(cat "$work/dump.sha256")" != "$expected" ]; then
  echo "build: mbdump.tar.bz2 SHA-256 mismatch; refusing the extracted tables" >&2
  exit 1
fi
phase extracted

# DUMP_TS from the directory name (UTC), the importer's timestamp shape.
ts="${MB_DUMP:0:4}-${MB_DUMP:4:2}-${MB_DUMP:6:2}T${MB_DUMP:9:2}:${MB_DUMP:11:2}:${MB_DUMP:13:2}Z"
DUMP_TS="$ts" MB_ETL_CONSUME_INPUTS=1 MB_ETL_GZIP_OUTPUT=1 \
  python3 /src/containers/mb-import/etl.py "$work/dump"
phase etl

curated="$work/dump/curated"
/tools/snapshot_build artist "$work/snap" "$curated/mbdump-artist.tsv.gz"
phase snapshot_artist
/tools/snapshot_build release "$work/snap" "$curated/mbdump-release.tsv.gz" "$curated/mbdump-artist.tsv.gz" 4
phase snapshot_release
/tools/snapshot_build recording "$work/snap" "$curated/mbdump-recording.tsv.gz" 4
phase snapshot_recording
phase done
