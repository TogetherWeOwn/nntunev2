#!/usr/bin/env bash
# Offline argv-hygiene acceptance tests for import.sh and replicate.sh.
#
# Contract under test (see import.sh header: "DATABASE_URL in env (never
# as argv)"): no child process of the importer or the replication script
# may receive DATABASE_URL, a password, or any credential-bearing
# connection string in its argv. Credentials must reach psql through a
# documented libpq mechanism instead:
#   * PG* environment variables (PGHOST/PGPORT/PGUSER/PGDATABASE/
#     PGPASSWORD/PGSSLMODE/...), where PGPASSWORD carries the *decoded*
#     password, or PGDATABASE carries a conninfo/URI value that libpq
#     expands (dbname expansion), or
#   * a password/service file referenced by PGPASSFILE/PGSERVICEFILE (or
#     the libpq defaults under $HOME), with owner-only permissions.
# Merely dropping the argv and leaving the connection information only in
# DATABASE_URL is NOT a pass: libpq does not read DATABASE_URL.
#
# The scripts run against fake psql/curl/mb-validate/python3 binaries
# that capture, per invocation: argv (one element per line), the libpq
# environment, stdin, and the contents+mode of any referenced credential
# file. Synthetic sentinel credentials are used throughout; no network
# and no real database are touched. Failure reports name the sentinel
# *label* and invocation index, never the captured value.
#
# Preserved behavior is pinned alongside hygiene: every psql call site
# (cursor insert/read, FK parent probe, chunk transaction, DLQ poison
# count, replication-state read/seed) must still run, ON_ERROR_STOP must
# survive, URI options (sslmode, sslrootcert, connect_timeout,
# application_name) must reach psql's effective configuration, and the
# scripts' exit behavior (import success, already-complete skip, FK
# orphan routing, replication fail-closed exit 2, gap refusal exit 1)
# must be unchanged.
set -uo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
container_dir="$(cd -- "$script_dir/.." && pwd)"
real_python3="$(command -v python3 || true)"
[[ -n "$real_python3" ]] || { echo "test-argv-hygiene: python3 not on PATH" >&2; exit 2; }

work_root="$(mktemp -d)"
trap 'rm -rf "$work_root"' EXIT
fakebin="$work_root/bin"
mkdir -p "$fakebin"

failures=0
passes=0
ok() { passes=$((passes + 1)); echo "ok: $1"; }
bad() { failures=$((failures + 1)); echo "FAIL: $1" >&2; }
check_eq() { # desc want got
  if [[ "$2" == "$3" ]]; then ok "$1 ($3)"; else bad "$1 (want $2, got $3)"; fi
}
check_file_contains() { # desc file fixed-string
  if grep -Fq -- "$3" "$2" 2>/dev/null; then ok "$1"; else bad "$1 (missing expected text in $(basename -- "$2"))"; fi
}
check_file_not_contains() { # desc file fixed-string
  if grep -Fq -- "$3" "$2" 2>/dev/null; then bad "$1 (unexpected text in $(basename -- "$2"))"; else ok "$1"; fi
}

# ---------------------------------------------------------------- fakes
cat >"$fakebin/psql" <<'FAKE_PSQL'
#!/usr/bin/env bash
set -uo pipefail
cap="${FAKE_CAPTURE_DIR:?FAKE_CAPTURE_DIR unset}"
mkdir -p "$cap"
n=0; [[ -f "$cap/psql.count" ]] && n="$(cat "$cap/psql.count")"
n=$((n + 1)); printf '%s' "$n" >"$cap/psql.count"
id="$(printf 'psql.%03d' "$n")"
printf '%s\n' "$@" >"$cap/$id.argv"
{
  for v in PGHOST PGHOSTADDR PGPORT PGUSER PGDATABASE PGPASSWORD PGSSLMODE \
           PGSSLROOTCERT PGSSLCERT PGSSLKEY PGCONNECT_TIMEOUT PGAPPNAME \
           PGOPTIONS PGSERVICE PGSERVICEFILE PGPASSFILE PGREQUIRESSL \
           PGCHANNELBINDING PGTARGETSESSIONATTRS DATABASE_URL; do
    printf '%s=%s\n' "$v" "${!v-__UNSET__}"
  done
} >"$cap/$id.env"
: >"$cap/$id.files"
for f in "${PGPASSFILE:-}" "${PGSERVICEFILE:-}" "${HOME:-/nonexistent}/.pgpass" "${HOME:-/nonexistent}/.pg_service.conf"; do
  [[ -n "$f" ]] || continue
  if [[ -f "$f" ]]; then
    { printf 'FILE %s MODE %s\n' "$f" "$(stat -c %a -- "$f" 2>/dev/null || echo unknown)"
      cat -- "$f"
      printf '\nENDOFFILE\n'; } >>"$cap/$id.files"
  elif [[ "$f" == "${PGPASSFILE:-}" || "$f" == "${PGSERVICEFILE:-}" ]]; then
    printf 'FILE %s MISSING\n' "$f" >>"$cap/$id.files"
  fi
done
sql=""
prev=""
for a in "$@"; do
  if [[ "$prev" == "-c" || "$prev" == "--command" ]]; then sql="$a"; fi
  case "$a" in
    --command=*) sql="${a#--command=}" ;;
    -c?*) sql="${a#-c}" ;;
  esac
  prev="$a"
done
if [[ -z "$sql" ]]; then
  cat >"$cap/$id.stdin"
  sql="$(cat "$cap/$id.stdin")"
else
  : >"$cap/$id.stdin"
fi
printf '%s' "$sql" >"$cap/$id.sql"
case "$sql" in
  *"SELECT lines_done FROM mb_import_cursor"*) printf '%s\n' "${FAKE_LINES_DONE:-0}" ;;
  *"SELECT count(*) FROM mb_import_dlq"*) printf '%s\n' "${FAKE_POISON_COUNT:-0}" ;;
  *"SELECT last_packet FROM mb_import_state"*) printf '%s\n' "${FAKE_LAST_PACKET:-0}" ;;
  *"CREATE TEMP TABLE want_parent"*)
    # Only report the orphan while its line_no is still among the queried
    # parents: track probes twice (one per parent) and a row split out by
    # the first probe is no longer in the second probe's parents file.
    if [[ -n "${FAKE_FK_BAD:-}" ]]; then
      parents_file="$(printf '%s' "$sql" | grep -o "FROM '[^']*'" | head -n 1 | sed "s/FROM '//; s/'$//")"
      if [[ -n "$parents_file" && -f "$parents_file" ]]; then
        if awk -F'\t' -v bad="$FAKE_FK_BAD" '$2 == bad { f = 1 } END { exit !f }' "$parents_file"; then
          printf '%s\n' "$FAKE_FK_BAD"
        fi
      else
        printf '%s\n' "$FAKE_FK_BAD"
      fi
    fi
    : ;;
  *"RETURNING lines_done"*)
    end="$(printf '%s' "$sql" | grep -o 'SET lines_done = [0-9]*' | grep -o '[0-9]*$' | head -n 1)"
    [[ "$end" =~ ^[0-9]+$ ]] || { echo "fake-psql: cannot parse chunk end" >&2; exit 3; }
    printf '%s\n' "$end" ;;
  *"INSERT INTO mb_import_cursor"*) : ;;
  *"UPDATE mb_import_state SET last_packet"*) : ;;
  *FAKE_CAPTURE_SELFTEST*) printf 'selftest-ok\n' ;;
  *) echo "fake-psql: unrecognised SQL (invocation $id)" >&2; exit 3 ;;
esac
FAKE_PSQL

cat >"$fakebin/curl" <<'FAKE_CURL'
#!/usr/bin/env bash
set -uo pipefail
cap="${FAKE_CAPTURE_DIR:?FAKE_CAPTURE_DIR unset}"
mkdir -p "$cap"
n=0; [[ -f "$cap/curl.count" ]] && n="$(cat "$cap/curl.count")"
n=$((n + 1)); printf '%s' "$n" >"$cap/curl.count"
printf '%s\n' "$@" >"$(printf '%s/curl.%03d.argv' "$cap" "$n")"
out=""
prev=""
for a in "$@"; do
  [[ "$prev" == "-o" || "$prev" == "--output" ]] && out="$a"
  case "$a" in --output=*) out="${a#--output=}" ;; -o?*) out="${a#-o}" ;; esac
  prev="$a"
done
[[ -n "$out" ]] && printf 'fake-packet-bytes\n' >"$out"
exit 0
FAKE_CURL

cat >"$fakebin/mb-validate" <<'FAKE_VALIDATE'
#!/usr/bin/env bash
set -uo pipefail
cap="${FAKE_CAPTURE_DIR:?FAKE_CAPTURE_DIR unset}"
mkdir -p "$cap"
n=0; [[ -f "$cap/validate.count" ]] && n="$(cat "$cap/validate.count")"
n=$((n + 1)); printf '%s' "$n" >"$cap/validate.count"
printf '%s\n' "$@" >"$(printf '%s/validate.%03d.argv' "$cap" "$n")"
in=""; valid_out=""; poison_out=""; offset=0
while (( $# > 0 )); do
  case "$1" in
    --in) in="$2"; shift 2 ;;
    --valid-out) valid_out="$2"; shift 2 ;;
    --poison-out) poison_out="$2"; shift 2 ;;
    --line-offset) offset="$2"; shift 2 ;;
    --table) shift 2 ;;
    *) echo "fake-validate: unknown arg $1" >&2; exit 3 ;;
  esac
done
awk -v off="$offset" '{ printf "%s\t%d\n", $0, off + NR }' "$in" >"$valid_out"
: >"$poison_out"
count="$(awk 'END { print NR+0 }' "$in")"
printf 'valid=%s poison=0\n' "$count"
FAKE_VALIDATE

cat >"$fakebin/python3" <<'FAKE_PY'
#!/usr/bin/env bash
set -uo pipefail
cap="${FAKE_CAPTURE_DIR:-}"
if [[ -n "$cap" ]]; then
  mkdir -p "$cap"
  n=0; [[ -f "$cap/python.count" ]] && n="$(cat "$cap/python.count")"
  n=$((n + 1)); printf '%s' "$n" >"$cap/python.count"
  printf '%s\n' "$@" >"$(printf '%s/python.%03d.argv' "$cap" "$n")"
fi
exec "${FAKE_REAL_PYTHON3:?FAKE_REAL_PYTHON3 unset}" "$@"
FAKE_PY
chmod +x "$fakebin/psql" "$fakebin/curl" "$fakebin/mb-validate" "$fakebin/python3"

# ------------------------------------------------------- credential cases
# Case "plain": ordinary sentinel password.
# Case "encoded": user and password carry characters that must be
# percent-encoded in a URI (@ : / % ? # [ ] space & ' \); libpq must end
# up with the decoded values, so a fix that copies the still-encoded
# userinfo into PGUSER/PGPASSWORD fails here.
url_case=""
db_user_raw=""; db_user_enc=""; db_pw_raw=""; db_pw_enc=""; db_pw_pgpass=""
db_host="db.invalid"; db_port="5433"; db_name="nntune_mb"
db_app="mb-import-sentinel-app"; db_ca="/nonexistent-test-ca/ca.pem"
database_url=""
set_url_case() {
  url_case="$1"
  if [[ "$url_case" == "plain" ]]; then
    db_user_raw="mb_svc_user"; db_user_enc="mb_svc_user"
    db_pw_raw="SENTINEL_PLAINTEXT_PW_x9"; db_pw_enc="SENTINEL_PLAINTEXT_PW_x9"
  else
    db_user_raw="mb@svc.example"; db_user_enc="mb%40svc.example"
    db_pw_raw="S3nt!nel@p:ss/w%rd?#[] &'\\"
    db_pw_enc="S3nt!nel%40p%3Ass%2Fw%25rd%3F%23%5B%5D%20%26%27%5C"
  fi
  db_pw_pgpass="${db_pw_raw//\\/\\\\}"
  db_pw_pgpass="${db_pw_pgpass//:/\\:}"
  database_url="postgresql://${db_user_enc}:${db_pw_enc}@${db_host}:${db_port}/${db_name}?sslmode=require&sslrootcert=${db_ca}&connect_timeout=7&application_name=${db_app}"
}

# ------------------------------------------------------------- assertions
# Sentinel labels->values are rebuilt per case; reports print labels only.
assert_no_sentinels_in_file() { # desc file
  local desc="$1" f="$2" leaked=""
  local -a labels=("full DATABASE_URL" "raw password" "percent-encoded password" "user:password pair")
  local -a values=("$database_url" "$db_pw_raw" "$db_pw_enc" "${db_user_enc}:${db_pw_enc}")
  local i
  for i in "${!labels[@]}"; do
    if grep -Fq -- "${values[$i]}" "$f" 2>/dev/null; then
      leaked="${leaked:+$leaked, }${labels[$i]}"
    fi
  done
  if [[ -n "$leaked" ]]; then
    bad "$desc: leaks into $(basename -- "$f"): $leaked"
  else
    ok "$desc: no credential sentinel in $(basename -- "$f")"
  fi
}

# Every captured argv (psql/curl/mb-validate/python3) in a capture dir:
# no sentinel, no conninfo URI, no password= conninfo keyword. The exact
# token --no-password is a legitimate psql flag, not a credential.
assert_argv_clean() { # capture-dir
  local cap="$1" f line base hits
  shopt -s nullglob
  local -a argv_files=("$cap"/*.argv)
  shopt -u nullglob
  if (( ${#argv_files[@]} == 0 )); then
    bad "argv hygiene: no child-process invocations captured in $cap (harness or script never ran a child)"
    return
  fi
  for f in "${argv_files[@]}"; do
    base="$(basename -- "$f")"
    assert_no_sentinels_in_file "argv hygiene [$url_case]" "$f"
    hits=0
    while IFS= read -r line || [[ -n "$line" ]]; do
      [[ "$line" == "--no-password" || "$line" == "-w" ]] && continue
      if [[ "$line" =~ postgres(ql)?:// ]] || [[ "$line" =~ [Pp][Aa][Ss][Ss][Ww][Oo][Rr][Dd][[:space:]]*= ]]; then
        hits=$((hits + 1))
      fi
    done <"$f"
    if (( hits > 0 )); then
      bad "argv hygiene [$url_case]: $base carries a conninfo URI or password= keyword ($hits element(s))"
    else
      ok "argv hygiene [$url_case]: $base has no conninfo URI / password= element"
    fi
  done
}

# Per-psql-invocation effective configuration: argv + libpq env (minus
# DATABASE_URL, which libpq ignores) + referenced credential files.
assert_psql_delivery() { # capture-dir
  local cap="$1" envf base id
  shopt -s nullglob
  local -a env_files=("$cap"/psql.*.env)
  shopt -u nullglob
  if (( ${#env_files[@]} == 0 )); then
    bad "psql delivery [$url_case]: no psql invocations captured"
    return
  fi
  for envf in "${env_files[@]}"; do
    base="$(basename -- "$envf" .env)"; id="$base"
    local combined="$work_root/.combined.$$"
    { cat -- "$cap/$id.argv"
      grep -v '^DATABASE_URL=' -- "$envf" || true
      cat -- "$cap/$id.files"; } >"$combined" 2>/dev/null
    # Connection target preserved.
    check_file_contains "psql delivery [$url_case] $id: host preserved" "$combined" "$db_host"
    check_file_contains "psql delivery [$url_case] $id: port preserved" "$combined" "$db_port"
    check_file_contains "psql delivery [$url_case] $id: dbname preserved" "$combined" "$db_name"
    if grep -Fq -- "$db_user_raw" "$combined" || grep '^PGDATABASE=' -- "$envf" | grep -Fq -- "$db_user_enc"; then
      ok "psql delivery [$url_case] $id: user preserved (decoded, or encoded inside PGDATABASE URI)"
    else
      bad "psql delivery [$url_case] $id: user missing from effective libpq configuration"
    fi
    # URI options / TLS preserved.
    if grep -Eq 'PGSSLMODE=require|sslmode[= ]require|PGREQUIRESSL=1' "$combined"; then
      ok "psql delivery [$url_case] $id: sslmode=require preserved"
    else
      bad "psql delivery [$url_case] $id: sslmode=require dropped from effective configuration"
    fi
    check_file_contains "psql delivery [$url_case] $id: sslrootcert preserved" "$combined" "$db_ca"
    if grep -Eq 'PGCONNECT_TIMEOUT=7|connect_timeout[= ]7' "$combined"; then
      ok "psql delivery [$url_case] $id: connect_timeout=7 preserved"
    else
      bad "psql delivery [$url_case] $id: connect_timeout=7 dropped from effective configuration"
    fi
    check_file_contains "psql delivery [$url_case] $id: application_name preserved" "$combined" "$db_app"
    # Password reaches psql via an allowed channel (never argv, covered
    # separately; never DATABASE_URL alone, which libpq ignores).
    local channels="$work_root/.channels.$$"
    { grep '^PGPASSWORD=' -- "$envf" || true; cat -- "$cap/$id.files"; } >"$channels" 2>/dev/null
    if grep -Fq -- "$db_pw_raw" "$channels" || grep -Fq -- "$db_pw_pgpass" "$channels"; then
      ok "psql delivery [$url_case] $id: decoded password delivered via PGPASSWORD or credential file"
    elif grep '^PGDATABASE=' -- "$envf" | grep -Fq -- "$db_pw_enc"; then
      ok "psql delivery [$url_case] $id: password delivered via PGDATABASE conninfo/URI expansion"
    else
      bad "psql delivery [$url_case] $id: password not delivered via any libpq mechanism (PGPASSWORD / pgpass / service file / PGDATABASE URI)"
    fi
    # Referenced credential files must be owner-only at call time.
    local modes m loose=0
    modes="$(grep -o 'MODE [0-9]*' -- "$cap/$id.files" 2>/dev/null | awk '{print $2}' || true)"
    for m in $modes; do
      case "$m" in 600|400|200|000|700) : ;; *) loose=1 ;; esac
    done
    if grep -q ' MISSING' -- "$cap/$id.files" 2>/dev/null; then
      bad "psql delivery [$url_case] $id: PGPASSFILE/PGSERVICEFILE points at a missing file"
    elif (( loose == 1 )); then
      bad "psql delivery [$url_case] $id: credential file has group/other permissions"
    else
      ok "psql delivery [$url_case] $id: credential files owner-only (or none used)"
    fi
    # psql's stdin SQL must not become the credential channel either.
    assert_no_sentinels_in_file "psql stdin [$url_case] $id" "$cap/$id.stdin"
    check_file_contains "psql invocation [$url_case] $id: ON_ERROR_STOP preserved" "$cap/$id.argv" "ON_ERROR_STOP=1"
    rm -f "$combined" "$channels"
  done
}

count_sql() { # capture-dir fixed-string -> prints count
  local cap="$1" needle="$2" total=0 f c
  shopt -s nullglob
  for f in "$cap"/psql.*.sql; do
    c="$(grep -Fo -- "$needle" "$f" 2>/dev/null | wc -l)"; total=$((total + c))
  done
  shopt -u nullglob
  printf '%s' "$total"
}
check_sql_at_least() { # desc capture-dir needle min
  local got; got="$(count_sql "$2" "$3")"
  if (( got >= $4 )); then ok "$1 ($got >= $4)"; else bad "$1 (want >= $4, got $got)"; fi
}

# After a run, no sentinel-bearing file may be left behind with
# group/other permissions anywhere under the scenario's HOME, TMPDIR,
# cwd or dump dir, and no sentinel at all in cwd/dump dir.
assert_no_leftover_secret_files() { # scenario-dir
  local sdir="$1" f mode rel
  local found_loose=0 found_stray=0
  while IFS= read -r -d '' f; do
    if grep -Fq -- "$db_pw_raw" "$f" 2>/dev/null || grep -Fq -- "$db_pw_enc" "$f" 2>/dev/null; then
      mode="$(stat -c %a -- "$f" 2>/dev/null || echo 777)"
      rel="${f#"$sdir"/}"
      case "$mode" in 600|400|200|000) : ;; *) found_loose=1; echo "leftover (loose perms $mode): $rel" >&2 ;; esac
      case "$rel" in cwd/*|dump/*) found_stray=1; echo "leftover (in cwd/dump): $rel" >&2 ;; esac
    fi
  done < <(find "$sdir/home" "$sdir/tmp" "$sdir/cwd" "$sdir/dump" -type f -print0 2>/dev/null)
  if (( found_loose == 0 )); then ok "leftover files [$url_case]: no group/other-readable sentinel file left behind"; else bad "leftover files [$url_case]: sentinel left in a group/other-readable file"; fi
  if (( found_stray == 0 )); then ok "leftover files [$url_case]: no sentinel file in cwd or dump dir"; else bad "leftover files [$url_case]: sentinel file left in cwd or dump dir"; fi
}

# ---------------------------------------------------------------- harness
make_dump() { # dump-dir : 2 valid rows per table
  local d="$1"; mkdir -p "$d"
  printf '00000000-0000-0000-0000-000000000001\tBand 1\t1, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n00000000-0000-0000-0000-000000000002\tBand 2\t2, Band\tGroup\tUS\t2024-05-01T12:00:00Z\n' >"$d/mbdump-artist.tsv"
  printf '00000000-0000-0000-0000-000000010001\t00000000-0000-0000-0000-000000000001\tAlbum 1\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n00000000-0000-0000-0000-000000010002\t00000000-0000-0000-0000-000000000002\tAlbum 2\tAlbum\t2024\t\\N\t2024-05-02T00:00:00Z\n' >"$d/mbdump-release.tsv"
  printf '00000000-0000-0000-0000-000000020001\t00000000-0000-0000-0000-000000010001\tTrack 1\t1\t180000\t2024-05-03T00:00:00Z\n00000000-0000-0000-0000-000000020002\t00000000-0000-0000-0000-000000010002\tTrack 2\t1\t180000\t2024-05-03T00:00:00Z\n' >"$d/mbdump-recording.tsv"
  printf '00000000-0000-0000-0000-000000010001\t00000000-0000-0000-0000-000000020001\t1\t1\t2024-05-03T00:00:00Z\n00000000-0000-0000-0000-000000010002\t00000000-0000-0000-0000-000000020002\t1\t1\t2024-05-03T00:00:00Z\n' >"$d/mbdump-track.tsv"
}

# run_import <name> ; scenario knobs via FAKE_* / DUMP_SEQ in the caller env
scenario_dir=""; capture_dir=""; run_exit=0
prep_scenario() { # name
  scenario_dir="$work_root/$1"
  capture_dir="$scenario_dir/capture"
  mkdir -p "$scenario_dir/home" "$scenario_dir/tmp" "$scenario_dir/cwd" "$capture_dir"
  make_dump "$scenario_dir/dump"
}
base_env_args=()
build_base_env() {
  base_env_args=(
    "PATH=$fakebin:/usr/local/bin:/usr/bin:/bin"
    "HOME=$scenario_dir/home"
    "TMPDIR=$scenario_dir/tmp"
    "FAKE_CAPTURE_DIR=$capture_dir"
    "FAKE_REAL_PYTHON3=$real_python3"
    "DATABASE_URL=$database_url"
  )
}
run_import() { # extra env assignments...
  build_base_env
  run_exit=0
  ( cd "$scenario_dir/cwd" && env -i "${base_env_args[@]}" "$@" \
      bash "$container_dir/import.sh" "$scenario_dir/dump" \
      >"$scenario_dir/stdout" 2>"$scenario_dir/stderr" </dev/null ) || run_exit=$?
}
run_replicate() { # packet-seq, extra env via caller FAKE_*
  build_base_env
  run_exit=0
  ( cd "$scenario_dir/cwd" && env -i "${base_env_args[@]}" \
      PACKET_SEQ="$1" PACKET_URL="https://example.invalid/packet-$1.tar.gz" \
      FAKE_LAST_PACKET="${FAKE_LAST_PACKET:-0}" \
      bash "$container_dir/replicate.sh" \
      >"$scenario_dir/stdout" 2>"$scenario_dir/stderr" </dev/null ) || run_exit=$?
}
common_post_checks() {
  assert_argv_clean "$capture_dir"
  assert_psql_delivery "$capture_dir"
  assert_no_sentinels_in_file "script output [$url_case]" "$scenario_dir/stdout"
  assert_no_sentinels_in_file "script output [$url_case]" "$scenario_dir/stderr"
  assert_no_leftover_secret_files "$scenario_dir"
}

# ------------------------------------------- harness positive control
echo "=== harness self-check: fake psql capture actually records argv ==="
prep_scenario "selfcheck"
build_base_env
selftest_out="$(env -i "${base_env_args[@]}" "$fakebin/psql" "marker-not-a-credential" -tAX -c "SELECT 'FAKE_CAPTURE_SELFTEST';" </dev/null)"
check_eq "fake psql self-check response" "selftest-ok" "$selftest_out"
check_file_contains "fake psql self-check argv captured" "$capture_dir/psql.001.argv" "marker-not-a-credential"

# ------------------------------------------------------------- scenarios
for case_name in plain encoded; do
  set_url_case "$case_name"

  echo "=== [$case_name] full import: every psql call site, chunk path ==="
  prep_scenario "import-full-$case_name"
  run_import FAKE_LINES_DONE=0 FAKE_POISON_COUNT=0 FAKE_LAST_PACKET=0 FAKE_FK_BAD= DUMP_SEQ=500
  check_eq "import full [$case_name]: exit" "0" "$run_exit"
  check_file_contains "import full [$case_name]: completes" "$scenario_dir/stdout" "import: done"
  check_file_contains "import full [$case_name]: seeds replication state" "$scenario_dir/stdout" "seeded mb_import_state.last_packet=500"
  check_sql_at_least "import full [$case_name]: cursor insert ran per table" "$capture_dir" "INSERT INTO mb_import_cursor" 4
  check_sql_at_least "import full [$case_name]: cursor read ran per table" "$capture_dir" "SELECT lines_done FROM mb_import_cursor" 4
  check_sql_at_least "import full [$case_name]: FK parent probe ran for release+recording+track(x2)" "$capture_dir" "CREATE TEMP TABLE want_parent" 4
  check_sql_at_least "import full [$case_name]: chunk transaction ran per table" "$capture_dir" "RETURNING lines_done" 4
  check_sql_at_least "import full [$case_name]: DLQ poison count ran per table" "$capture_dir" "SELECT count(*) FROM mb_import_dlq" 4
  check_sql_at_least "import full [$case_name]: replication state read ran" "$capture_dir" "SELECT last_packet FROM mb_import_state" 1
  check_sql_at_least "import full [$case_name]: replication state seed ran" "$capture_dir" "UPDATE mb_import_state SET last_packet" 1
  if [[ -s "$capture_dir/validate.count" ]]; then ok "import full [$case_name]: validator child ran"; else bad "import full [$case_name]: validator child never ran"; fi
  common_post_checks

  echo "=== [$case_name] replicate: fail-closed pending-apply exit ==="
  prep_scenario "replicate-pending-$case_name"
  FAKE_LAST_PACKET=41 run_replicate 42
  check_eq "replicate pending [$case_name]: exit" "2" "$run_exit"
  check_file_contains "replicate pending [$case_name]: NOT applied reported" "$scenario_dir/stderr" "NOT applied"
  check_sql_at_least "replicate pending [$case_name]: state read ran" "$capture_dir" "SELECT last_packet FROM mb_import_state" 1
  check_eq "replicate pending [$case_name]: no state write" "0" "$(count_sql "$capture_dir" "UPDATE mb_import_state")"
  check_file_contains "replicate pending [$case_name]: curl fetched the packet URL" "$capture_dir/curl.001.argv" "https://example.invalid/packet-42.tar.gz"
  common_post_checks
done

set_url_case plain

echo "=== already-complete import: skip path, older DUMP_SEQ never rewinds ==="
prep_scenario "import-complete-plain"
run_import FAKE_LINES_DONE=2 FAKE_POISON_COUNT=0 FAKE_LAST_PACKET=7 FAKE_FK_BAD= DUMP_SEQ=5
check_eq "import complete: exit" "0" "$run_exit"
check_file_contains "import complete: skip reported" "$scenario_dir/stdout" "already complete"
check_file_contains "import complete: older seed leaves state untouched" "$scenario_dir/stderr" "leaving state untouched"
check_eq "import complete: no chunk transaction" "0" "$(count_sql "$capture_dir" "RETURNING lines_done")"
check_eq "import complete: no state seed" "0" "$(count_sql "$capture_dir" "UPDATE mb_import_state SET last_packet")"
check_sql_at_least "import complete: all-poison guard still consulted DLQ" "$capture_dir" "SELECT count(*) FROM mb_import_dlq" 4
if [[ -e "$capture_dir/validate.count" ]]; then bad "import complete: validator ran on the skip path"; else ok "import complete: validator not run on the skip path"; fi
common_post_checks

echo "=== FK orphan import: orphan rows routed, import still succeeds ==="
prep_scenario "import-fk-plain"
run_import FAKE_LINES_DONE=0 FAKE_POISON_COUNT=1 FAKE_LAST_PACKET=0 FAKE_FK_BAD=2
check_eq "import fk: exit" "0" "$run_exit"
check_file_contains "import fk: orphan routing reported (artist parent)" "$scenario_dir/stdout" "fk_missing_artist"
check_file_contains "import fk: orphan routing reported (release parent)" "$scenario_dir/stdout" "fk_missing_release"
check_file_contains "import fk: completes" "$scenario_dir/stdout" "import: done"
check_file_not_contains "import fk: no DUMP_SEQ seed claimed" "$scenario_dir/stdout" "seeded mb_import_state.last_packet"
if [[ -s "$capture_dir/python.count" ]]; then ok "import fk: python3 FK-split child ran (argv captured)"; else bad "import fk: python3 FK-split child never ran"; fi
common_post_checks

echo "=== replicate: sequence gap refused ==="
prep_scenario "replicate-gap-plain"
FAKE_LAST_PACKET=41 run_replicate 99
check_eq "replicate gap: exit" "1" "$run_exit"
check_file_contains "replicate gap: refusal reported" "$scenario_dir/stderr" "refusing packet 99 (expected 42"
common_post_checks

echo
echo "test-argv-hygiene: $passes passed, $failures failed"
if (( failures != 0 )); then
  echo "test-argv-hygiene: FAILED" >&2
  exit 1
fi
echo "test-argv-hygiene: all checks passed"
