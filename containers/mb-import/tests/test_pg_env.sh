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
  # A missing ValueError guard turns a clean refusal into a Python
  # traceback, whose message can carry a token from the URL.
  if grep -Fq 'Traceback' "$err_file"; then
    bad "$1: error output is a Python traceback"
  else
    ok "$1: error output is a clean refusal"
  fi
}
check_err_guidance() { # desc -- refusal just run must name the remedy
  if grep -Fq 'percent-encode' "$err_file"; then
    ok "$1: error output names the percent-encode remedy"
  else
    bad "$1: error output does not name the percent-encode remedy"
  fi
}
check_err_contains() { # desc fixed-text -- refusal just run must name it
  if grep -Fq -- "$2" "$err_file"; then
    ok "$1: error output names $2"
  else
    bad "$1: error output does not name $2"
  fi
}
reset_pg() {
  unset PGHOST PGHOSTADDR PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE \
        PGSSLROOTCERT PGCONNECT_TIMEOUT PGAPPNAME PGSERVICE PGPASSFILE \
        PGSSLCRL PGSSLCRLDIR PGSSLSNI PGSSLCERTMODE PGSSLNEGOTIATION \
        PGSSLCOMPRESSION PGSSLMINPROTOCOLVERSION PGSSLMAXPROTOCOLVERSION \
        PGGSSENCMODE PGGSSLIB PGGSSDELEGATION PGKRBSRVNAME PGREQUIREAUTH \
        PGREQUIREPEER PGLOADBALANCEHOSTS PGMINPROTOCOLVERSION \
        PGMAXPROTOCOLVERSION
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

reset_pg
DATABASE_URL='postgres://u:p@h/db?sslcrl=/crl.pem&sslcrldir=/crls&sslsni=1&sslcertmode=require&sslnegotiation=direct&sslcompression=0&ssl_min_protocol_version=TLSv1.3&ssl_max_protocol_version=TLSv1.3&gssencmode=disable&gsslib=gssapi&gssdelegation=0&krbsrvname=postgres&require_auth=scram-sha-256&requirepeer=mb&load_balance_hosts=disable&min_protocol_version=3.0&max_protocol_version=3.2'
run_ok "URI with the extended libpq option set"
check_eq "sslcrl" '/crl.pem' "${PGSSLCRL-}"
check_eq "sslcrldir" '/crls' "${PGSSLCRLDIR-}"
check_eq "sslsni" '1' "${PGSSLSNI-}"
check_eq "sslcertmode" 'require' "${PGSSLCERTMODE-}"
check_eq "sslnegotiation" 'direct' "${PGSSLNEGOTIATION-}"
check_eq "sslcompression" '0' "${PGSSLCOMPRESSION-}"
check_eq "ssl_min_protocol_version" 'TLSv1.3' "${PGSSLMINPROTOCOLVERSION-}"
check_eq "ssl_max_protocol_version" 'TLSv1.3' "${PGSSLMAXPROTOCOLVERSION-}"
check_eq "gssencmode" 'disable' "${PGGSSENCMODE-}"
check_eq "gsslib" 'gssapi' "${PGGSSLIB-}"
check_eq "gssdelegation" '0' "${PGGSSDELEGATION-}"
check_eq "krbsrvname" 'postgres' "${PGKRBSRVNAME-}"
check_eq "require_auth" 'scram-sha-256' "${PGREQUIREAUTH-}"
check_eq "requirepeer" 'mb' "${PGREQUIREPEER-}"
check_eq "load_balance_hosts" 'disable' "${PGLOADBALANCEHOSTS-}"
check_eq "min_protocol_version" '3.0' "${PGMINPROTOCOLVERSION-}"
check_eq "max_protocol_version" '3.2' "${PGMAXPROTOCOLVERSION-}"

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@h/db?service=staging'
run_refused "URI with a service parameter" 'SENTINELPW'

reset_pg
DATABASE_URL='host=h service=staging password=SENTINELPW'
run_refused "keyword conninfo with a service parameter" 'SENTINELPW'

reset_pg
export PGSERVICE='staging' # inherited means exported, as a container env is
DATABASE_URL='postgres://u:SENTINELPW@h/db'
run_refused "inherited PGSERVICE" 'SENTINELPW'

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@h1,h2/db'
run_refused "multi-host URI" 'SENTINELPW'

reset_pg
DATABASE_URL='postgres://u:p@h:SENTINELPORT/db'
run_refused "URI with an invalid port" 'SENTINELPORT'

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@[::1/db'
run_refused "URI with an unterminated IPv6 host" 'SENTINELPW'

reset_pg
DATABASE_URL='postgres://u:SENTINEL#pw@h/db'
run_refused "URI with a raw '#' in the password" 'SENTINEL'
check_err_guidance "URI with a raw '#' in the password"

reset_pg
DATABASE_URL='postgres://u:SENTINEL?pw@h/db'
run_refused "URI with a raw '?' in the password" 'SENTINEL'
check_err_guidance "URI with a raw '?' in the password"

reset_pg
DATABASE_URL='postgres://u:p+ss@h/db?application_name=mb+import'
run_ok "URI with literal '+' in password and query value"
check_eq "literal '+' in password kept" 'p+ss' "${PGPASSWORD-}"
check_eq "literal '+' in query value kept" 'mb+import' "${PGAPPNAME-}"

reset_pg
PGUSER='keep-user'
PGPASSWORD='keep-pw'
DATABASE_URL='postgres://:@h/db'
run_ok "URI with empty userinfo"
check_eq "empty URI user leaves PGUSER untouched" 'keep-user' "${PGUSER-}"
check_eq "empty URI password leaves PGPASSWORD untouched" 'keep-pw' "${PGPASSWORD-}"

reset_pg
PGPASSWORD='keep-pw'
DATABASE_URL='postgres://newuser:@h/db'
run_ok "URI with empty password only"
check_eq "URI user still set" 'newuser' "${PGUSER-}"
check_eq "empty URI password leaves PGPASSWORD untouched" 'keep-pw' "${PGPASSWORD-}"

reset_pg
DATABASE_URL='postgres://u:%FF@h/db'
run_ok "URI with a non-UTF-8 percent-encoded password byte"
check_eq "%FF reaches PGPASSWORD as byte 0xFF" $'\xff' "${PGPASSWORD-}"

reset_pg
DATABASE_URL='postgresql+asyncpg://u:SENTINELPW@h/db?ssl=require'
run_refused "URI with an unsupported scheme" 'SENTINELPW'
check_err_contains "URI with an unsupported scheme" 'scheme is not supported'

reset_pg
DATABASE_URL='host=h SENTINELKEY=1'
run_refused "keyword conninfo with an unknown keyword" 'SENTINELKEY'

reset_pg
DATABASE_URL='postgres://u:p@h/db?SENTINELKEY=1'
run_refused "URI with an unknown query parameter" 'SENTINELKEY'

reset_pg
DATABASE_URL='host=h keepalives=0 password=SENTINELPW'
run_refused "keyword conninfo with a known unmappable keyword" 'SENTINELPW'
check_err_contains "keyword conninfo with a known unmappable keyword" "'keepalives'"

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@h/db?keepalives_idle=10'
run_refused "URI with a known unmappable query parameter" 'SENTINELPW'
check_err_contains "URI with a known unmappable query parameter" "'keepalives_idle'"

reset_pg
DATABASE_URL='postgres://u:123?SENTINELKEY=abc@h/db'
run_refused "URI with a raw '?' and '=' in the password" 'SENTINELKEY'
check_err_guidance "URI with a raw '?' and '=' in the password"

reset_pg
DATABASE_URL='postgres://u:p@h/db#SENTINELFRAG'
run_refused "URI with a raw '#' in the database name" 'SENTINELFRAG'
check_err_guidance "URI with a raw '#' in the database name"

reset_pg
DATABASE_URL='postgres://u:p@h/db?application_name=a#SENTINELFRAG'
run_refused "URI with a raw '#' in a query value" 'SENTINELFRAG'
check_err_guidance "URI with a raw '#' in a query value"

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@h/db#'
run_refused "URI with a trailing raw '#' after the database name" 'SENTINELPW'
check_err_guidance "URI with a trailing raw '#' after the database name"

reset_pg
DATABASE_URL='postgres://u:SENTINELPW@h/db?application_name=a#'
run_refused "URI with a trailing raw '#' after a query value" 'SENTINELPW'
check_err_guidance "URI with a trailing raw '#' after a query value"

reset_pg
DATABASE_URL='postgres://u:p@h?application_name=a@b'
run_ok "pathless URI with '@' in a query value"
check_eq "pathless query host" 'h' "${PGHOST-}"
check_eq "'@' in query value kept" 'a@b' "${PGAPPNAME-}"

echo
echo "test-pg-env: $passes passed, $failures failed"
if (( failures > 0 )); then
  echo "test-pg-env: FAILED" >&2
  exit 1
fi
echo "test-pg-env: all checks passed"
