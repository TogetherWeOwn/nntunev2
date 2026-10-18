#!/usr/bin/env bash
# Semantic-reference rejection tests for the mb-import ETL.
#
# containers/mb-import/etl.py refuses correctly shaped rows whose joins
# dangle (missing lookup/credit/track/medium references); test_etl.sh only
# covers malformed file/argument shape and snapshot publication/survival, so
# this companion suite pins four broken-reference refusal diagnostics
# (missing artist_type, release credit-name rows, track recording, medium
# release) with independent synthetic fixture copies; every mutation rewrites
# a single TSV field with awk (tab counts unchanged) and the field-count
# guard below proves the run
# reached the join check instead of failing on shape. Each broken case must
# exit nonzero with its existing diagnostic before any curated snapshot is
# published. Uses local tiny fixture copies only; no network, no database.
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
container_dir="$(cd -- "$script_dir/.." && pwd)"
fixtures="$script_dir/fixtures"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

# fail <message>: report a failed expectation and stop the suite.
fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# assert_field_count <file> <expected>: prove the mutated fixture keeps valid
# shape so a failure below must come from the intended join check.
assert_field_count() {
  local file="$1" expected="$2" bad
  bad="$(awk -F'\t' -v want="$expected" 'NF != want { print FILENAME": row "NR": "NF" fields" }' "$file")"
  [[ -z "$bad" ]] || fail "$bad"
}

# assert_refused <case-dir> <err-file> <diagnostic>: the ETL must already have
# exited nonzero (checked by the caller), the diagnostic must be present, and
# no curated snapshot may have been published.
assert_refused() {
  local case_dir="$1" err_file="$2" diagnostic="$3"
  grep -qFx "etl: $diagnostic" "$err_file" || fail "missing diagnostic '$diagnostic' in $err_file"
  [[ ! -e "$case_dir/curated" ]] || fail "curated published for broken case $case_dir"
  if compgen -G "$case_dir/.curated-*" > /dev/null; then fail "snapshot published for broken case $case_dir"; fi
}

# Positive control: the unchanged fixture still imports cleanly.
cp -R "$fixtures/mbdump" "$temp_dir/control"
DUMP_TS=2025-02-04T04:05:06Z "$container_dir/etl.sh" "$temp_dir/control"
[[ -L "$temp_dir/control/curated" ]] || fail 'control run published no curated symlink'
for table in artist release recording track; do
  [[ -s "$temp_dir/control/curated/mbdump-$table.tsv" ]] || fail "control run missing mbdump-$table.tsv"
done

# Case 1: artist references a missing artist_type.
cp -R "$fixtures/mbdump" "$temp_dir/missing-artist-type"
awk -F'\t' 'BEGIN { OFS="\t" } $1 == 10 { $11 = 99 } { print }' \
  "$temp_dir/missing-artist-type/artist" > "$temp_dir/artist.tmp"
mv "$temp_dir/artist.tmp" "$temp_dir/missing-artist-type/artist"
assert_field_count "$temp_dir/missing-artist-type/artist" 19
if DUMP_TS=2025-02-04T04:05:07Z "$container_dir/etl.sh" "$temp_dir/missing-artist-type" 2>"$temp_dir/missing-artist-type.err"; then
  fail 'expected missing-artist_type refusal'
fi
assert_refused "$temp_dir/missing-artist-type" "$temp_dir/missing-artist-type.err" \
  'artist 10 references missing artist_type 99'

# Case 2: release has no artist-credit-name rows.
cp -R "$fixtures/mbdump" "$temp_dir/missing-credit"
awk -F'\t' 'BEGIN { OFS="\t" } $1 == 30 { $4 = 999 } { print }' \
  "$temp_dir/missing-credit/release" > "$temp_dir/release.tmp"
mv "$temp_dir/release.tmp" "$temp_dir/missing-credit/release"
assert_field_count "$temp_dir/missing-credit/release" 14
if DUMP_TS=2025-02-04T04:05:08Z "$container_dir/etl.sh" "$temp_dir/missing-credit" 2>"$temp_dir/missing-credit.err"; then
  fail 'expected missing-credit-name refusal'
fi
assert_refused "$temp_dir/missing-credit" "$temp_dir/missing-credit.err" \
  'release 30 has no artist-credit-name rows'

# Case 3: track references a missing recording (its medium still exists).
cp -R "$fixtures/mbdump" "$temp_dir/missing-recording"
awk -F'\t' 'BEGIN { OFS="\t" } $1 == 70 { $3 = 9999 } { print }' \
  "$temp_dir/missing-recording/track" > "$temp_dir/track.tmp"
mv "$temp_dir/track.tmp" "$temp_dir/missing-recording/track"
assert_field_count "$temp_dir/missing-recording/track" 12
if DUMP_TS=2025-02-04T04:05:09Z "$container_dir/etl.sh" "$temp_dir/missing-recording" 2>"$temp_dir/missing-recording.err"; then
  fail 'expected missing-recording refusal'
fi
assert_refused "$temp_dir/missing-recording" "$temp_dir/missing-recording.err" \
  'track 70 references a missing medium or recording'

# Case 4: medium references a missing release (its tracks still resolve).
cp -R "$fixtures/mbdump" "$temp_dir/missing-release"
awk -F'\t' 'BEGIN { OFS="\t" } $1 == 60 { $2 = 9999 } { print }' \
  "$temp_dir/missing-release/medium" > "$temp_dir/medium.tmp"
mv "$temp_dir/medium.tmp" "$temp_dir/missing-release/medium"
assert_field_count "$temp_dir/missing-release/medium" 9
if DUMP_TS=2025-02-04T04:05:10Z "$container_dir/etl.sh" "$temp_dir/missing-release" 2>"$temp_dir/missing-release.err"; then
  fail 'expected missing-release refusal'
fi
assert_refused "$temp_dir/missing-release" "$temp_dir/missing-release.err" \
  'medium 60 references a missing release'

echo 'mb-import ETL reference-graph tests: PASS'
