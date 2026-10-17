#!/usr/bin/env bash
# Offline unit tests for pg-env.sh: DATABASE_URL -> libpq PG* env.
#
# pg-env.sh must keep accepting every form the old `psql "$DATABASE_URL"`
# connection argument accepted (a postgres URI, a libpq keyword/value
# conninfo string, or a bare database name), decode percent-encoded and
# quoted/escaped credentials exactly once, preserve URI query options,
# leave PG* variables the input does not specify untouched (libpq
# precedence parity), and refuse malformed or unmappable input without
# ever printing a credential value. Synthetic values only; no network,
# no database.
set -uo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../pg-env.sh
. "$script_dir/../pg-env.sh"

failures=0
passes=0
ok() { passes=$((passes + 1)); echo "ok: $1"; }
bad() { failures=$((failures + 1)); echo "FAIL: $1" >&2; }
check_eq() { # desc want got
  if [[ "$2" == "$3" ]]; then ok "$1"; else bad "$1 (want [$2], got [$3])"; fi
}

err_file="$(mktemp)"
trap 'rm -f "$err_file"' EXIT

run_ok() { # desc -- expects configure to succeed; caller checks PG* vars
  export DATABASE_URL # configure unsets it, so re-export for every case
  if configure_pg_env_from_database_url 2>"$err_file"; then
    ok "$1: accepted"
  else
    bad "$1: refused ($(cat "$err_file"))"
  fi
}
run_refused() { # desc sentinel-value-that-must-not-print
  export DATABASE_URL # configure unsets it, so re-export for every case
  if configure_pg_env_from_database_url 2>"$err_file"; then
    bad "$1: accepted but must be refused"
  else
    ok "$1: refused"
  fi
  if grep -Fq -- "$2" "$err_file"; then
    bad "$1: error output leaks the refused value"
  else
    ok "$1: error output carries no credential value"
  fi
}
reset_pg() {
  unset PGHOST PGHOSTADDR PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE \
        PGSSLROOTCERT PGCONNECT_TIMEOUT PGAPPNAME PGSERVICE PGPASSFILE
}

reset_pg
DATABASE_URL='postgresql://mb%40svc.example:S3nt%40p%3Ass@db.invalid:5433/nntune_mb?sslmode=require&sslrootcert=/ca.pem&connect_timeout=7&application_name=mb-import'
run_ok "URI with percent-encoded userinfo and query options"
check_eq "URI user decoded once" 'mb@svc.example' "${PGUSER-}"
check_eq "URI password decoded once" 'S3nt@p:ss' "${PGPASSWORD-}"
check_eq "URI host" 'db.invalid' "${PGHOST-}"
check_eq "URI port" '5433' "${PGPORT-}"
check_eq "URI dbname" 'nntune_mb' "${PGDATABASE-}"
check_eq "URI sslmode" 'require' "${PGSSLMODE-}"
check_eq "URI sslrootcert" '/ca.pem' "${PGSSLROOTCERT-}"
check_eq "URI connect_timeout" '7' "${PGCONNECT_TIMEOUT-}"
check_eq "URI application_name" 'mb-import' "${PGAPPNAME-}"
check_eq "DATABASE_URL unset after configure" '' "${DATABASE_URL-}"

reset_pg
DATABASE_URL='host=localhost port=5432 user=postgres dbname=postgres'
run_ok "keyword/value conninfo"
check_eq "keyword host" 'localhost' "${PGHOST-}"
check_eq "keyword port" '5432' "${PGPORT-}"
check_eq "keyword user" 'postgres' "${PGUSER-}"
check_eq "keyword dbname" 'postgres' "${PGDATABASE-}"

reset_pg
DATABASE_URL="host = h1 password='sp ace\\'q' dbname=db"
run_ok "keyword conninfo with quoted value"
check_eq "keyword quoted password unescaped once" "sp ace'q" "${PGPASSWORD-}"

reset_pg
DATABASE_URL='password=sp\ ace host=h'
run_ok "keyword conninfo with backslash-escaped space"
check_eq "keyword unquoted escape" 'sp ace' "${PGPASSWORD-}"

reset_pg
DATABASE_URL='justdb'
run_ok "bare database name"
check_eq "bare name becomes PGDATABASE" 'justdb' "${PGDATABASE-}"

reset_pg
PGHOST='preexisting-host'
DATABASE_URL='postgres:///music'
run_ok "URI without host"
check_eq "unspecified host leaves PGHOST untouched" 'preexisting-host' "${PGHOST-}"
check_eq "dbname still set" 'music' "${PGDATABASE-}"

reset_pg
DATABASE_URL='postgres://u:p@h/db?bogusopt=SENTINELVAL'
run_refused "URI with unmappable query parameter" 'SENTINELVAL'

reset_pg
DATABASE_URL='host=h bogus=SENTINELVAL'
run_refused "keyword conninfo with unmappable keyword" 'SENTINELVAL'

reset_pg
DATABASE_URL='host=h password'
run_refused "keyword conninfo missing '='" 'password'

reset_pg
DATABASE_URL="password='SENTINELVAL"
run_refused "keyword conninfo unterminated quote" 'SENTINELVAL'

reset_pg
DATABASE_URL='my database'
run_ok "bare database name with whitespace"
check_eq "whitespace name kept verbatim" 'my database' "${PGDATABASE-}"

reset_pg
DATABASE_URL=''
run_refused "empty DATABASE_URL" 'SENTINELVAL'

echo
echo "test-pg-env: $passes passed, $failures failed"
if (( failures > 0 )); then
  echo "test-pg-env: FAILED" >&2
  exit 1
fi
echo "test-pg-env: all checks passed"
