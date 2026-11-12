#!/usr/bin/env bash
# Run the schema boundary test against a container created solely for this run.
# No externally supplied database connection is accepted.
set -euo pipefail

if [ "$#" -ne 0 ] || [ "${DATABASE_URL+x}" = x ]; then
  echo 'external database targets and arguments are not accepted' >&2
  exit 1
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
tmp="$(mktemp -d)"
cleanup() {
  if [ -s "$tmp/container-id" ]; then
    id="$(<"$tmp/container-id")"
    if [[ "$id" =~ ^[0-9a-f]{64}$ ]]; then
      env -i PATH=/usr/bin:/bin /usr/bin/docker rm -fv "$id" >/dev/null
    fi
  fi
  rm -rf -- "$tmp"
}
trap cleanup EXIT

docker_clean() { env -i PATH=/usr/bin:/bin /usr/bin/docker "$@"; }
docker_clean run -d --cidfile "$tmp/container-id" \
  -e POSTGRES_PASSWORD=ci-throwaway -e POSTGRES_DB=nntune_ci \
  -p 127.0.0.1::5432 \
  postgres:18@sha256:74935e72241653ca55e0414067e6d8763aceb8a810eb51b452253ec3dcfc4336 >/dev/null
id="$(<"$tmp/container-id")"
[[ "$id" =~ ^[0-9a-f]{64}$ ]]
for _ in {1..60}; do
  if docker_clean exec "$id" pg_isready -h 127.0.0.1 -U postgres -d nntune_ci >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker_clean exec "$id" pg_isready -h 127.0.0.1 -U postgres -d nntune_ci >/dev/null

psql_run() {
  docker_clean exec -i -e PGOPTIONS='-c search_path=public,pg_catalog' "$id" \
    psql -U postgres -X -q -v ON_ERROR_STOP=1 "$@"
}
for db in nntune_app_ci nntune_mirror_ci; do
  psql_run -d nntune_ci -c "CREATE DATABASE $db"
done

python3 - "$root" > "$tmp/migrations" <<'PYEOF'
import sys, tomllib
from pathlib import Path
root = Path(sys.argv[1])
with (root / 'migrations/manifest.toml').open('rb') as fh:
    migrations = tomllib.load(fh)['migration']
for target in ('app', 'mirror'):
    files = [m['file'] for m in migrations if m['target'] == target]
    if not files:
        sys.exit(f'no {target}-target migrations in manifest')
    for name in files:
        if Path(name).name != name or '\n' in name or '\t' in name:
            sys.exit('invalid migration filename')
        print(f'{target}\t{name}')
PYEOF
while IFS=$'\t' read -r target name; do
  printf 'migrate %s: %s\n' "$target" "$name"
  psql_run -d "nntune_${target}_ci" < "$root/migrations/$name"
done < "$tmp/migrations"

port="$(docker_clean port "$id" 5432/tcp)"
[[ "$port" =~ ^127\.0\.0\.1:([0-9]+)$ ]]
port="${BASH_REMATCH[1]}"
NNTUNE_TEST_APP_DATABASE_URL="postgresql://postgres:ci-throwaway@127.0.0.1:$port/nntune_app_ci" \
NNTUNE_TEST_MIRROR_DATABASE_URL="postgresql://postgres:ci-throwaway@127.0.0.1:$port/nntune_mirror_ci" \
  cargo test --locked --manifest-path "$root/workers/api/Cargo.toml" --features postgres-tests --test meter_meta_store_postgres
