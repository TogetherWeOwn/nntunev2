#!/usr/bin/env bash
# Offline admission checks: the stub never contacts PostgreSQL.
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
dir="$(mktemp -d)"
trap 'rm -rf -- "$dir"' EXIT
# shellcheck disable=SC2016 # The stub evaluates these variables, not this script.
printf '%s\n' '#!/usr/bin/env bash' '[[ ${PGHOSTADDR+x} != x && ${PGSERVICE+x} != x && "$1" == "postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci?hostaddr=127.0.0.1" ]] || exit 99' 'exit 42' >"$dir/psql"
chmod +x "$dir/psql"
url='postgresql://postgres:ci-throwaway@localhost:5432/nntune_ci'
run() {
  local expected="$1"; shift
  local status=0
  env -i PATH="$dir:$PATH" GITHUB_ACTIONS=true RUNNER_ENVIRONMENT=github-hosted "$@" bash "$root/workers/api/tests/storage_digest_reservation_race.sh" </dev/null >/dev/null 2>&1 || status=$?
  [[ "$status" == "$expected" ]] || { printf 'expected exit %s, got %s\n' "$expected" "$status" >&2; exit 1; }
}
run 42 DATABASE_URL="$url" PGHOSTADDR=192.0.2.1 PGSERVICE=other
run 1 DATABASE_URL=postgresql://example.com/nntune_ci
run 1 DATABASE_URL="${url}?hostaddr=192.0.2.1"
run 1 DATABASE_URL='host=localhost dbname=nntune_ci'
run 1 DATABASE_URL="$url" GITHUB_ACTIONS=false
run 1 DATABASE_URL="$url" RUNNER_ENVIRONMENT=self-hosted
run 1
