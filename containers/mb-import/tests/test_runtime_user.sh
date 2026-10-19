#!/usr/bin/env bash
# Pin: the mb-import runtime stage must run as a non-root user.
#
# The batch job only reads /job, writes mktemp scratch under world-writable
# /tmp (or the mounted dump directory), and talks to Postgres over TCP, so
# the runtime stage must end (after its last RUN/COPY/ADD) with a non-root
# USER that owns /job. Fails if the runtime stage has no USER after its
# last mutation instruction, if that USER names root, or if no chown/--chown
# grants the user /job (the integration suite writes fixtures under /job).
# Run in CI (mb-import-etl job);
# the Dockerfile test stage is intentionally unchanged. Optional argv: path
# to the Dockerfile under test (defaults to containers/mb-import/Dockerfile).
set -uo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
dockerfile="${1:-$script_dir/../Dockerfile}"

failures=0
ok() { echo "ok: $1"; }
bad() { failures=$((failures + 1)); echo "FAIL: $1" >&2; }

[[ -f "$dockerfile" ]] || { bad "Dockerfile not found at $dockerfile"; exit 1; }

# last_mutation: line number of the last RUN/COPY/ADD in the runtime stage
# (0 when the stage has none). last_user/last_user_value: same for USER.
parsed="$(awk '
  BEGIN { in_runtime = 0; mutation = 0; user_line = 0; user_value = "" }
  /^[[:space:]]*FROM[[:space:]]+/ {
    if (in_runtime) exit
    stage = $0
    sub(/^[[:space:]]*FROM[[:space:]]+[^[:space:]]+[[:space:]]+/, "", stage)
    if (stage ~ /^[Aa][Ss][[:space:]]+runtime([[:space:]]|$)/) in_runtime = 1
    next
  }
  in_runtime && /^[[:space:]]*(RUN|COPY|ADD)([[:space:]]|$)/ { mutation = NR }
  in_runtime && /^[[:space:]]*USER([[:space:]]|$)/ {
    user_line = NR
    line = $0
    sub(/^[[:space:]]*USER[[:space:]]+/, "", line)
    split(line, fields, /[[:space:]]+/)
    user_value = fields[1]
  }
  END {
    if (!in_runtime) { print "NO_STAGE"; exit 0 }
    printf "%d %d %s\n", mutation, user_line, user_value
  }
' "$dockerfile")"

if [[ "$parsed" == "NO_STAGE" ]]; then
  bad "no 'AS runtime' stage in $dockerfile"
  exit 1
fi

last_mutation="${parsed%% *}"
rest="${parsed#* }"
last_user_line="${rest%% *}"
last_user_value="${rest#* }"

if [[ -z "$last_user_value" ]]; then
  bad "runtime stage has no USER (last RUN/COPY/ADD at line $last_mutation)"
  exit 1
fi
if (( last_user_line < last_mutation )); then
  bad "runtime stage USER ($last_user_value, line $last_user_line) is before the last RUN/COPY/ADD (line $last_mutation)"
  exit 1
fi

user_name="${last_user_value%%:*}"
group_name=""
[[ "$last_user_value" == *:* ]] && group_name="${last_user_value#*:}"
if [[ -z "$user_name" || "$user_name" == "root" || "$user_name" == "0" \
  || "$group_name" == "root" || "$group_name" == "0" ]]; then
  bad "runtime stage USER '$last_user_value' (line $last_user_line) is root"
  exit 1
fi

# The runtime user must own /job: the CI integration suite bind-mounts its
# runner into /job and writes fixtures next to it, so a USER without /job
# ownership breaks the shipped-image suite at container runtime.
if [[ ! "$last_user_value" =~ ^[A-Za-z0-9_.-]+(:[A-Za-z0-9_.-]+)?$ ]]; then
  bad "runtime stage USER '$last_user_value' (line $last_user_line) is not a plain user[:group]"
  exit 1
fi
user_ere="${user_name//./\\.}"
stage_text="$(awk '
  /^[[:space:]]*FROM[[:space:]]/ {
    if (in_runtime) exit
    if ($0 ~ /[Aa][Ss][[:space:]]+runtime([[:space:]]|$)/) in_runtime = 1
    next
  }
  in_runtime { print }
' "$dockerfile")"
# Strip trailing comments so a `# chown ... /job` remark cannot satisfy the
# gate, and anchor /job with a boundary so /jobbery-style paths cannot.
if ! grep -v '^[[:space:]]*#' <<<"$stage_text" | sed 's/#.*//' \
  | grep -Eq "chown.*${user_ere}[^[:space:]]*.*/job([[:space:]/\"']|$)"; then
  bad "runtime stage grants '$user_name' no /job ownership (need chown or --chown covering /job)"
  exit 1
fi

ok "runtime stage runs as non-root '$last_user_value' (USER line $last_user_line, after last RUN/COPY/ADD line $last_mutation) with /job ownership"
exit "$failures"
