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
diff -u "$expected/mbdump-release.tsv" "$temp_dir/mbdump/curated/mbdump-release.tsv"
diff -u "$expected/mbdump-recording.tsv" "$temp_dir/mbdump/curated/mbdump-recording.tsv"
# The fixture recording appears on both releases and on a second medium
# of the first: the membership file must keep every appearance in
# (release, medium, track) order, not just the canonical one the
# recording file carries.
diff -u "$expected/mbdump-track.tsv" "$temp_dir/mbdump/curated/mbdump-track.tsv"

first_snapshot="$(readlink "$temp_dir/mbdump/curated")"
DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump"
second_snapshot="$(readlink "$temp_dir/mbdump/curated")"
[[ "$first_snapshot" != "$second_snapshot" ]]
for table in artist release recording track; do
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
# snapshot, its symlink target and all four TSV hashes untouched.
published_target="$(readlink "$temp_dir/mbdump/curated")"
published_hashes="$temp_dir/published.sha256"
( cd "$temp_dir/mbdump/curated" && sha256sum mbdump-artist.tsv mbdump-release.tsv mbdump-recording.tsv mbdump-track.tsv > "$published_hashes" )

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
for table in artist release recording track; do
  grep -q '2025-02-03T04:05:08Z$' "$temp_dir/mbdump/curated/mbdump-$table.tsv"
done

echo 'mb-import ETL fixture tests: PASS'
