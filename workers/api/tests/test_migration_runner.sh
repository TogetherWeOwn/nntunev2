#!/usr/bin/env bash
# Exercise the scratch runner with a stub, never a database.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf -- "$dir"' EXIT
# shellcheck disable=SC2016 # These variables are evaluated by the stub.
printf '%s\n' '#!/usr/bin/env bash' 'printf "%s\n" "$*" >>"$PSQL_LOG"' 'exit "${PSQL_EXIT:-0}"' >"$dir/psql"
chmod +x "$dir/psql"

# A non-hosted scratch run must finish, applying migrations and SQL tests.
env -i PATH="$dir:$PATH" DATABASE_URL=postgresql://scratch/db PSQL_LOG="$dir/calls" \
  bash "$root/workers/api/tests/run_migration_tests.sh" >"$dir/output"
grep -q 'migration tests passed' "$dir/output"
grep -q 'migrations/0001_' "$dir/calls"
grep -q 'workers/api/tests/sql/' "$dir/calls"

# A psql error must still stop the runner before the success message.
status=0
env -i PATH="$dir:$PATH" DATABASE_URL=postgresql://scratch/db PSQL_LOG="$dir/calls" PSQL_EXIT=42 \
  bash "$root/workers/api/tests/run_migration_tests.sh" >"$dir/output" || status=$?
[[ "$status" == 42 ]] || { printf 'expected psql failure 42, got %s\n' "$status" >&2; exit 1; }
if grep -q 'migration tests passed' "$dir/output"; then
  printf 'runner reported success after psql failure\n' >&2; exit 1
fi
