#!/usr/bin/env bash
# Exercise the scratch runner with a stub, never a database.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf -- "$dir"' EXIT
cat >"$dir/psql" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$PSQL_LOG"
if [[ -n "${PSQL_EXIT:-}" ]]; then
  exit "$PSQL_EXIT"
fi
case " $* " in
  *' -At '*)
    while IFS= read -r line; do
      case "$line" in
        "SELECT 'reader-ready';") printf '%s\n' reader-ready ;;
        '\q') exit 0 ;;
      esac
    done
    exit 0 ;;
  *' -1 -v VERBOSITY=verbose '*)
    printf 'ERROR: 55P03: could not obtain lock on relation\n' >&2
    exit 3 ;;
  *' -1 -v purge_case=grants '*) reason='cache grants are not empty' ;;
  *' -1 -v purge_case=vault '*) reason='a vault owner lacks the account marker' ;;
  *' -1 -v purge_case=owner '*|*' -1 -v purge_case=owner_admin '*|*' -1 -v purge_case=owner_deleting '*)
    reason='provider owners would lose reaping protection' ;;
  *) exit 0 ;;
esac
printf 'ERROR: legacy provider purge refused: %s\n' "$reason" >&2
exit 3
STUB
chmod +x "$dir/psql"

# A non-hosted scratch run must finish, applying migrations and SQL tests.
env -i PATH="$dir:$PATH" DATABASE_URL=postgresql://scratch/db PSQL_LOG="$dir/calls" \
  bash "$root/workers/api/tests/run_migration_tests.sh" >"$dir/output"
grep -q 'migration tests passed' "$dir/output"
grep -q 'migrations/0001_' "$dir/calls"
grep -q 'workers/api/tests/sql/' "$dir/calls"
for table in users cache_objects; do
  grep -Fq "purge with a reader on $table: ERROR: 55P03: could not obtain lock on relation" "$dir/output"
done
for reason in 'cache grants are not empty' 'a vault owner lacks the account marker' \
  'provider owners would lose reaping protection'; do
  grep -Fq "legacy provider purge refused: $reason" "$dir/output"
done
for fixture in grants vault owner owner_admin owner_deleting; do
  grep -Fq "purge guard ($fixture): ERROR: legacy provider purge refused:" "$dir/output"
done
grep -Fq -- '-v purge_case=allow -c BEGIN;' "$dir/calls"

# A psql error must still stop the runner before the success message.
status=0
env -i PATH="$dir:$PATH" DATABASE_URL=postgresql://scratch/db PSQL_LOG="$dir/calls" PSQL_EXIT=42 \
  bash "$root/workers/api/tests/run_migration_tests.sh" >"$dir/output" || status=$?
[[ "$status" == 42 ]] || { printf 'expected psql failure 42, got %s\n' "$status" >&2; exit 1; }
if grep -q 'migration tests passed' "$dir/output"; then
  printf 'runner reported success after psql failure\n' >&2; exit 1
fi
