#!/usr/bin/env bash
# Exercise the workflow's history options against disposable Git repositories.
set -euo pipefail

scratch=$(mktemp -d)
trap 'rm -rf -- "$scratch"' EXIT

# Use a test-only rule and a generated marker, not a real provider format.
marker="fake-gitleaks-fixture-$(git hash-object --stdin <<< 'history scan test')"
printf '%s\n' '[[rules]]' 'id = "fixture-marker"' \
  "regex = 'fake-gitleaks-fixture-[a-f0-9]{40}'" > "$scratch/fixture.toml"

init_repo() {
  git init -q --initial-branch=main "$1"
  git -C "$1" config user.name 'History Scan Test'
  git -C "$1" config user.email 'fixture@example.invalid'
}

expect_finding() {
  local repo=$1 file=$2 status=0 commit
  commit=$(git -C "$repo" rev-parse HEAD)
  "$GITLEAKS_BIN" git "$repo" --redact --exit-code 1 \
    --log-opts="$GITLEAKS_LOG_OPTS" --ignore-gitleaks-allow \
    --config "$scratch/fixture.toml" --report-format json \
    --report-path "$repo.json" || status=$?
  # A scanner error also can exit nonzero; require the expected finding.
  if [ "$status" -ne 1 ] || ! jq -e --arg file "$file" --arg commit "$commit" '
    length == 1 and .[0].RuleID == "fixture-marker"
    and .[0].File == $file and .[0].Commit == $commit
  ' "$repo.json" > /dev/null; then
    printf '%s: expected one finding in %s (scanner exit: %s)\n' "${repo##*/}" "$file" "$status" >&2
    return 1
  fi
  printf '%s: expected history finding confirmed\n' "${repo##*/}"
}

repo="$scratch/no-diff"
init_repo "$repo"
printf '%s\n' 'fixture.txt -diff' > "$repo/.gitattributes"
printf '%s\n' "$marker" > "$repo/fixture.txt"
git -C "$repo" add .gitattributes fixture.txt
git -C "$repo" commit -qm 'Add text fixture with a diff attribute'
expect_finding "$repo" fixture.txt

repo="$scratch/merge-resolution"
init_repo "$repo"
printf '%s\n' 'base' > "$repo/fixture.txt"
git -C "$repo" add fixture.txt
git -C "$repo" commit -qm 'Add common ancestor'
git -C "$repo" switch -qc side
printf '%s\n' 'side' > "$repo/fixture.txt"
git -C "$repo" commit -qam 'Edit on side'
git -C "$repo" switch -q main
printf '%s\n' 'main' > "$repo/fixture.txt"
git -C "$repo" commit -qam 'Edit on main'
if git -C "$repo" merge --no-edit side || [ -z "$(git -C "$repo" ls-files -u -- fixture.txt)" ]; then
  printf '%s\n' 'merge-resolution: expected a conflict in fixture.txt' >&2
  exit 1
fi
printf '%s\n' "$marker" > "$repo/fixture.txt"
git -C "$repo" add fixture.txt
git -C "$repo" commit -qm 'Resolve conflict with the test marker'
expect_finding "$repo" fixture.txt
