#!/usr/bin/env bash
# Prepare two independently migrated disposable stores for the meter-metadata
# boundary test: nntune_app_ci receives only the app-target migrations from
# migrations/manifest.toml, nntune_mirror_ci only the mirror-target ones, in
# manifest order. DATABASE_URL must point at the scratch admin database
# (CI's postgres service container, db nntune_ci). Never point it at a
# shared database: this script drops and recreates both stores.
set -euo pipefail

: "${DATABASE_URL:?DATABASE_URL (scratch admin PostgreSQL) is required}"
command -v psql >/dev/null || { echo "psql not found" >&2; exit 1; }

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
psql_run() { PGOPTIONS='-c search_path=public,pg_catalog' psql "$DATABASE_URL" -X -q -v ON_ERROR_STOP=1 "$@"; }

for db in nntune_app_ci nntune_mirror_ci; do
  psql_run -c "DROP DATABASE IF EXISTS $db WITH (FORCE)"
  psql_run -c "CREATE DATABASE $db"
done

app_url="${DATABASE_URL%/*}/nntune_app_ci"
mirror_url="${DATABASE_URL%/*}/nntune_mirror_ci"

# Apply each target's migration history to its own empty store, filtered by
# the manifest's declared target (the same source run_migration_tests.sh
# uses). No cross-side relations can exist: an app-store relation for a
# mirror table (or vice versa) would fail here on the missing predecessor.
python3 - "$root" "$app_url" "$mirror_url" <<'PYEOF'
import subprocess, sys, tomllib
root, app_url, mirror_url = sys.argv[1], sys.argv[2], sys.argv[3]
with open(f"{root}/migrations/manifest.toml", "rb") as fh:
    manifest = tomllib.load(fh)["migration"]
for target, url in (("app", app_url), ("mirror", mirror_url)):
    files = [m["file"] for m in manifest if m["target"] == target]
    if not files:
        print(f"no {target}-target migrations in manifest", file=sys.stderr)
        sys.exit(1)
    for name in files:
        print(f"migrate {target}: {name}")
        env = {"PGOPTIONS": "-c search_path=public,pg_catalog", "PATH": "/usr/bin:/bin"}
        subprocess.run(
            ["psql", url, "-X", "-q", "-v", "ON_ERROR_STOP=1", "-f", f"{root}/migrations/{name}"],
            check=True, env=env,
        )
print("migrated stores prepared")
PYEOF
