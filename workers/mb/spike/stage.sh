#!/usr/bin/env bash
# R2 snapshot spike, staging half: upload, deploy the spike Worker, bench,
# tear down. Runs only from `main` via workflow_dispatch (stage=true).
#
# Usage: stage.sh up <snap-dir> | bench <mbids-dir> <golden.tsv> <out.json> | down
# Env: MB_DUMP, WRANGLER_VERSION, CLOUDFLARE_API_TOKEN, CLOUDFLARE_ACCOUNT_ID.
#
# Uses only the existing CI token (Workers Scripts + Workers R2 Storage
# edit); no new credential. The bucket gets a 14-day expiry rule on
# `snap/` (zero idle cost even if teardown is skipped). The spike Worker
# `nntune-mb-snap-staging` serves only requests carrying a random per-run
# bench token (a Worker secret that dies with the Worker), and `down`
# deletes the Worker and the uploaded objects.
set -euo pipefail

BUCKET=nntunev2-mb-snap-staging
WORKER=nntune-mb-snap-staging
: "${MB_DUMP:?}" "${WRANGLER_VERSION:?}"
PREFIX="snap/${MB_DUMP}/"
here=$(cd "$(dirname "$0")" && pwd)
state="${RUNNER_TEMP:-/tmp}/mb-snap-stage"
mkdir -p "$state"
wrangler() { npx --yes "wrangler@${WRANGLER_VERSION}" "$@"; }
RULE=expire-spike-snapshots

has_expiry_rule() {
  local rules
  rules=$(wrangler r2 bucket lifecycle list "$BUCKET") || return 1
  grep -q "$RULE" <<< "$rules" && grep -q "snap/" <<< "$rules"
}

up() {
  local snap=$1
  if ! wrangler r2 bucket info "$BUCKET" > /dev/null 2>&1; then
    wrangler r2 bucket create "$BUCKET"
  fi
  # No upload without a confirmed expiry rule: it is the backstop when
  # teardown fails or the run is cancelled. Add only if missing (reruns).
  if ! has_expiry_rule; then
    wrangler r2 bucket lifecycle add "$BUCKET" "$RULE" snap/ --expire-days 14
    has_expiry_rule || { echo "stage: cannot confirm the $RULE expiry rule; not uploading" >&2; exit 1; }
  fi
  for file in "$snap"/*.idx "$snap"/*.dat.*; do
    wrangler r2 object put "$BUCKET/$PREFIX$(basename "$file")" --file "$file" --remote > /dev/null
    basename "$file" >> "$state/objects.txt"
  done

  local config="$here/../wrangler.spike.toml"
  cat > "$config" <<EOF
name = "${WORKER}"
main = "build/worker/shim.mjs"
compatibility_date = "2026-10-01"
workers_dev = true
preview_urls = false

[build]
command = "cargo install -q worker-build --locked && worker-build --release"

[vars]
MB_BACKEND = "r2snapshot"
MB_SNAPSHOT_PREFIX = "${PREFIX}"

[[r2_buckets]]
binding = "MB_SNAPSHOT"
bucket_name = "${BUCKET}"
EOF
  (cd "$here/.." && wrangler deploy -c wrangler.spike.toml) | tee "$state/deploy.log"
  touch "$state/deployed"
  local token
  token=$(openssl rand -hex 32)
  echo "::add-mask::$token"
  printf '%s' "$token" > "$state/token"
  printf '%s' "$token" | (cd "$here/.." && wrangler secret put MB_SNAPSHOT_BENCH_TOKEN -c wrangler.spike.toml)
  local url
  url=$(grep -oE "https://${WORKER}\.[a-z0-9-]+\.workers\.dev" "$state/deploy.log" | head -1)
  [ -n "$url" ] || { echo "stage: no workers.dev URL in deploy output" >&2; exit 1; }
  echo "::add-mask::$url"
  printf '%s' "$url" > "$state/url"
  for _ in $(seq 1 30); do
    curl -fsS "$url/healthz" > /dev/null 2>&1 && return 0
    sleep 2
  done
  echo "stage: spike Worker did not become healthy" >&2
  exit 1
}

bench() {
  MB_SNAPSHOT_BENCH_TOKEN=$(cat "$state/token") \
    python3 "$here/bench.py" "$(cat "$state/url")" "$1" "$2" "$3"
}

down() {
  # Attempt every delete, then fail the step if any failed.
  local failed=0
  if [ -f "$state/deployed" ]; then
    wrangler delete --name "$WORKER" --force || { echo "stage: Worker delete failed" >&2; failed=$((failed + 1)); }
  fi
  if [ -f "$state/objects.txt" ]; then
    while read -r name; do
      wrangler r2 object delete "$BUCKET/$PREFIX$name" --remote > /dev/null < /dev/null \
        || { echo "stage: delete failed: $PREFIX$name" >&2; failed=$((failed + 1)); }
    done < "$state/objects.txt"
  fi
  if [ "$failed" -gt 0 ]; then
    echo "stage: teardown incomplete, $failed delete(s) failed (the $RULE rule expires snap/ objects in 14 days)" >&2
    exit 1
  fi
}

case "${1:-}" in
  up) up "$2" ;;
  bench) bench "$2" "$3" "$4" ;;
  down) down ;;
  *) echo "usage: stage.sh up <snap-dir> | bench <mbids-dir> <golden.tsv> <out.json> | down" >&2; exit 2 ;;
esac
