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

first_snapshot="$(readlink "$temp_dir/mbdump/curated")"
DUMP_TS=2025-02-03T04:05:07Z "$container_dir/etl.sh" "$temp_dir/mbdump"
second_snapshot="$(readlink "$temp_dir/mbdump/curated")"
[[ "$first_snapshot" != "$second_snapshot" ]]
for table in artist release recording; do
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

echo 'mb-import ETL fixture tests: PASS'
