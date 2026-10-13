#!/usr/bin/env bash
# mbdump dumps -> Postgres COPY into nntunev2-mb (idempotent, resumable).
# Usage: DATABASE_URL=<url> ./import.sh <dump-dir>
# Never echo $DATABASE_URL; pass it only via env/secret store.
set -euo pipefail

if [[ "${1:-}" == "--help" || $# -lt 1 ]]; then
  cat <<'USAGE'
Usage: DATABASE_URL=<postgres-url> ./import.sh <dump-dir>

Imports mbdump TSV dumps (artist, release, recording) with COPY ... FROM
STDIN, staging per file so a rerun resumes at the first missing file.
Requires: psql on PATH, DATABASE_URL in env (never as argv).
USAGE
  exit 0
fi

dump_dir="$1"
: "${DATABASE_URL:?set DATABASE_URL via secret store, never on the command line}"

for table in artist release recording; do
  tsv="$dump_dir/mbdump-$table.tsv"
  if [[ ! -f "$tsv" ]]; then
    echo "import: skip $table (no $tsv)"
    continue
  fi
  echo "import: loading $table from $tsv"
  # Staging table keeps reruns idempotent: INSERT ... ON CONFLICT DO NOTHING.
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<SQL
CREATE TEMP TABLE stage (LIKE mb_${table}) ON COMMIT DROP;
SQL
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
    -c "\\copy stage FROM '$tsv' WITH (FORMAT text)"
  psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<SQL
INSERT INTO mb_${table} SELECT * FROM stage
ON CONFLICT (mbid) DO UPDATE SET
  updated_at = EXCLUDED.updated_at;
SQL
done

echo "import: done"
