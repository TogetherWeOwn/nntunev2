#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
container_dir="$(cd -- "$script_dir/.." && pwd)"
fixtures="$script_dir/fixtures"
expected="$script_dir/expected"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

cp -R "$fixtures/mbdump" "$temp_dir/mbdump"
DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/mbdump"
diff -u "$expected/mbdump-artist.tsv" "$temp_dir/mbdump/curated/mbdump-artist.tsv"
diff -u "$expected/mbdump-release_group.tsv" "$temp_dir/mbdump/curated/mbdump-release_group.tsv"
diff -u "$expected/mbdump-release.tsv" "$temp_dir/mbdump/curated/mbdump-release.tsv"
diff -u "$expected/mbdump-recording.tsv" "$temp_dir/mbdump/curated/mbdump-recording.tsv"
diff -u "$expected/mbdump-url.tsv" "$temp_dir/mbdump/curated/mbdump-url.tsv"
diff -u "$expected/mbdump-artist_url.tsv" "$temp_dir/mbdump/curated/mbdump-artist_url.tsv"
diff -u "$expected/mbdump-recording_url.tsv" "$temp_dir/mbdump/curated/mbdump-recording_url.tsv"
diff -u "$expected/mbdump-recording_music_video.tsv" "$temp_dir/mbdump/curated/mbdump-recording_music_video.tsv"
diff -u "$expected/mbdump-recording_isrc.tsv" "$temp_dir/mbdump/curated/mbdump-recording_isrc.tsv"

# The preserved core-dump cover rows share endpoints with music-video rows.
# Give one excluded edge a nonexistent endpoint so a mistaken admission cannot
# hide behind duplicate canonicalization of the existing music-video pair.
cp -R "$fixtures/mbdump" "$temp_dir/excluded-cover"
python3 - "$temp_dir/excluded-cover/l_recording_recording" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
lines = path.read_text().splitlines(keepends=True)
if sum(line.startswith('323\t214\t50\t51\t') for line in lines) != 1:
    raise ValueError('expected exactly one excluded cover row')
lines = [line.replace('323\t214\t50\t51\t', '323\t214\t50\t99999\t') for line in lines]
path.write_text(''.join(lines))
PY
DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/excluded-cover"
diff -u "$expected/mbdump-recording_music_video.tsv" "$temp_dir/excluded-cover/curated/mbdump-recording_music_video.tsv"

# Even an excluded relationship must have the upstream COPY row width.
cp -R "$fixtures/mbdump" "$temp_dir/malformed-cover"
printf '325\t214\t50\n' >> "$temp_dir/malformed-cover/l_recording_recording"
if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/malformed-cover" 2>"$temp_dir/malformed-cover.err"; then
  echo 'expected malformed excluded relationship failure' >&2
  exit 1
fi
grep -q 'row 6: expected 9 fields, got 3' "$temp_dir/malformed-cover.err"
# The fixture recording appears on both releases and on a second medium
# of the first: the membership file must keep every appearance in
# (release, medium, track) order, not just the canonical one the
# recording file carries.
diff -u "$expected/mbdump-track.tsv" "$temp_dir/mbdump/curated/mbdump-track.tsv"

first_snapshot="$(readlink "$temp_dir/mbdump/curated")"
DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump"
second_snapshot="$(readlink "$temp_dir/mbdump/curated")"
[[ "$first_snapshot" != "$second_snapshot" ]]
for table in artist release_group release recording track url artist_url recording_url recording_music_video recording_isrc; do
  grep -q '2025-02-03T04:05:07Z$' "$temp_dir/mbdump/curated/mbdump-$table.tsv"
done

if "$container_dir/etl.sh" >"$temp_dir/usage.out" 2>&1; then
  echo 'expected wrong-argument failure' >&2
  exit 1
fi

if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/missing" 2>"$temp_dir/missing.err"; then
  echo 'expected missing-directory failure' >&2
  exit 1
fi
grep -q 'input directory does not exist' "$temp_dir/missing.err"

cp -R "$fixtures/mbdump" "$temp_dir/incomplete"
rm "$temp_dir/incomplete/track"
if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/incomplete" 2>"$temp_dir/incomplete.err"; then
  echo 'expected missing-input-file failure' >&2
  exit 1
fi
grep -q 'missing input file: .*\/track' "$temp_dir/incomplete.err"

cp -R "$fixtures/mbdump" "$temp_dir/bad"
printf 'broken\n' > "$temp_dir/bad/artist"
if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/bad" 2>"$temp_dir/bad.err"; then
  echo 'expected wrong-field-count failure' >&2
  exit 1
fi
grep -q 'expected 19 fields, got 1' "$temp_dir/bad.err"

# A kept relationship edge whose URL row is absent from the dump fails
# closed: the projection joins would otherwise drop the edge silently
# while the nonempty-output guard still passes. URL id 91 backs the
# artist-url edge to "free streaming" in the fixture.
cp -R "$fixtures/mbdump" "$temp_dir/dangling-url"
grep -v '^91	' "$temp_dir/dangling-url/url" > "$temp_dir/dangling-url/url.tmp"
mv "$temp_dir/dangling-url/url.tmp" "$temp_dir/dangling-url/url"
if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/dangling-url" 2>"$temp_dir/dangling-url.err"; then
  echo 'expected dangling-url failure' >&2
  exit 1
fi
grep -q 'artist-url link references missing url' "$temp_dir/dangling-url.err"

if DUMP_TS= "$container_dir/etl.sh" "$temp_dir/mbdump" 2>"$temp_dir/timestamp.err"; then
  echo 'expected missing timestamp failure' >&2
  exit 1
fi
grep -q 'DUMP_TS must be an ISO-8601 timestamp' "$temp_dir/timestamp.err"

cp -R "$fixtures/mbdump" "$temp_dir/unknown-country-missing"
rm "$temp_dir/unknown-country-missing/release_unknown_country"
if DUMP_TS=2025-02-03T04:05:06Z "$container_dir/etl.sh" "$temp_dir/unknown-country-missing" 2>"$temp_dir/unknown-country-missing.err"; then
  echo 'expected missing release_unknown_country failure' >&2
  exit 1
fi
grep -q 'missing input file: .*release_unknown_country' "$temp_dir/unknown-country-missing.err"

# Publication survival: identical same-DUMP_TS reruns reuse the published
# snapshot byte-for-byte; changed or failed reruns must leave the published
# snapshot, its symlink target and all ten TSV hashes untouched.
published_target="$(readlink "$temp_dir/mbdump/curated")"
published_hashes="$temp_dir/published.sha256"
( cd "$temp_dir/mbdump/curated" && sha256sum mbdump-artist.tsv mbdump-release_group.tsv mbdump-release.tsv mbdump-recording.tsv mbdump-track.tsv mbdump-url.tsv mbdump-artist_url.tsv mbdump-recording_url.tsv mbdump-recording_music_video.tsv mbdump-recording_isrc.tsv > "$published_hashes" )

# Identical input at the same DUMP_TS reuses the existing snapshot.
DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump"
[[ "$(readlink "$temp_dir/mbdump/curated")" == "$published_target" ]]
( cd "$temp_dir/mbdump/curated" && sha256sum -c "$published_hashes" )

# Changed output at the same DUMP_TS is refused, not published.
cp "$temp_dir/mbdump/artist" "$temp_dir/artist.bak"
sed -i 's/First Artist/First Artist Renamed/' "$temp_dir/mbdump/artist"
if DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump" 2>"$temp_dir/changed.err"; then
  echo 'expected same-timestamp changed-output refusal' >&2
  exit 1
fi
grep -q 'already has different curated output' "$temp_dir/changed.err"
[[ "$(readlink "$temp_dir/mbdump/curated")" == "$published_target" ]]
( cd "$temp_dir/mbdump/curated" && sha256sum -c "$published_hashes" )
cp "$temp_dir/artist.bak" "$temp_dir/mbdump/artist"

# A malformed/failed rerun preserves the symlink target and all TSV hashes.
printf 'broken\n' > "$temp_dir/mbdump/artist"
if DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump" 2>"$temp_dir/preserved.err"; then
  echo 'expected malformed rerun failure' >&2
  exit 1
fi
grep -q 'expected 19 fields, got 1' "$temp_dir/preserved.err"
[[ "$(readlink "$temp_dir/mbdump/curated")" == "$published_target" ]]
( cd "$temp_dir/mbdump/curated" && sha256sum -c "$published_hashes" )
cp "$temp_dir/artist.bak" "$temp_dir/mbdump/artist"

# A subsequent valid newer timestamp publishes successfully.
DUMP_TS=2025-02-03T04:05:08Z "$container_dir/etl.sh" "$temp_dir/mbdump"
new_target="$(readlink "$temp_dir/mbdump/curated")"
[[ "$new_target" != "$published_target" ]]
for table in artist release_group release recording track url artist_url recording_url recording_music_video recording_isrc; do
  grep -q '2025-02-03T04:05:08Z$' "$temp_dir/mbdump/curated/mbdump-$table.tsv"
done

echo 'mb-import ETL fixture tests: PASS'
