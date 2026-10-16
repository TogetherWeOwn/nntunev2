#!/usr/bin/env bash
# Deploy gate: assert the HYPERDRIVE config for the API has query caching disabled.
#
# Auth reads (sessions, credentials, users, challenges, vaults) are plain
# SELECTs through the HYPERDRIVE binding (see src/worker_entry.rs). Cloudflare
# Hyperdrive caches non-mutating queries by default and never invalidates on
# writes; sessions/authentication are the documented case for a separate
# `--caching-disabled` config. With caching on, logout/revoke-all is delayed
# up to ~75s, refresh rotation 401s, and sign_count advances fail.
#
# Usage:
#   ./check_hyperdrive_cache.sh --config-id <hyperdrive-id> [--allow-skip]
#   CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=... ./check_hyperdrive_cache.sh --config-id <id>
#   ./check_hyperdrive_cache.sh --self-test   # shim-based exit-code pins (no network, no secrets)
#
# Fails closed when credentials are absent (exit 3): a gate that cannot run
# must not report success. Secret-less CI passes --allow-skip explicitly to
# skip instead; real staging/prod deploys MUST provide credentials and MUST
# NOT pass --allow-skip (the Worker additionally refuses to serve traffic
# without HYPERDRIVE_CACHING_DISABLED=1, set only after this gate passes).
# Fails when caching is enabled or the config cannot be read
# with credentials present.
set -euo pipefail

CONFIG_ID=""
ALLOW_SKIP=0
SELF_TEST=0
while [ $# -gt 0 ]; do
  case "$1" in
    --config-id)
      CONFIG_ID="${2:-}"
      shift 2
      ;;
    --config-id=*)
      CONFIG_ID="${1#--config-id=}"
      shift
      ;;
    --allow-skip)
      ALLOW_SKIP=1
      shift
      ;;
    --self-test)
      SELF_TEST=1
      shift
      ;;
    -h|--help)
      sed -n '1,24p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $1 (want --config-id <id> [--allow-skip] [--self-test])" >&2
      exit 2
      ;;
  esac
done

run_self_tests() {
  # Pin the gate's exit codes with a stubbed `curl` (no network, no secrets).
  # An exported shell function (not a PATH shim: some sandboxes manage PATH)
  # serves $SHIM_BODY_FILE with HTTP status $SHIM_HTTP_CODE.
  curl() {
    local out="" want_out=0 arg
    for arg in "$@"; do
      if [ "$want_out" = "1" ]; then out="$arg"; want_out=0; continue; fi
      if [ "$arg" = "-o" ]; then want_out=1; fi
    done
    cat "$SHIM_BODY_FILE" > "$out"
    printf '%s' "$SHIM_HTTP_CODE"
  }
  export -f curl
  local failures=0
  local tmp
  tmp="$(mktemp -d)"

  expect() {
    # $1 = case name, $2 = wanted rc, rest = gate args.
    # Globals: `code` (shim HTTP status); body in $tmp/body.json;
    # credentials via CLOUDFLARE_* exports set by the caller.
    local name="$1" want="$2"
    shift 2
    local rc=0
    SHIM_BODY_FILE="$tmp/body.json" SHIM_HTTP_CODE="$code" \
      bash "$0" "$@" >/dev/null 2>&1 || rc=$?
    if [ "$rc" = "$want" ]; then
      echo "self-test PASS: $name (rc $rc)"
    else
      echo "self-test FAIL: $name (want rc $want, got rc $rc)" >&2
      failures=$((failures + 1))
    fi
  }

  export CLOUDFLARE_API_TOKEN="test-token" CLOUDFLARE_ACCOUNT_ID="test-account"
  echo '{"result":{"caching":{"disabled":true}}}' > "$tmp/body.json"; code=200
  expect "caching disabled passes" 0 --config-id cfg-1
  echo '{"result":{"caching":{"disabled":false}}}' > "$tmp/body.json"; code=200
  expect "caching enabled fails" 1 --config-id cfg-1
  echo '{"result":{}}' > "$tmp/body.json"; code=200
  expect "missing caching block fails" 1 --config-id cfg-1
  echo '{"result":null}' > "$tmp/body.json"; code=200
  expect "null result fails" 1 --config-id cfg-1
  echo '{}' > "$tmp/body.json"; code=403
  expect "http error fails" 1 --config-id cfg-1
  unset CLOUDFLARE_API_TOKEN CLOUDFLARE_ACCOUNT_ID
  expect "missing credentials fail closed" 3 --config-id cfg-1
  expect "missing credentials with --allow-skip pass" 0 --config-id cfg-1 --allow-skip
  export CLOUDFLARE_API_TOKEN="test-token"
  expect "half credentials fail closed" 3 --config-id cfg-1

  rm -rf "$tmp"
  if [ "$failures" != "0" ]; then
    echo "self-test: $failures case(s) failed" >&2
    return 1
  fi
  echo "self-test: all cases pass"
  return 0
}

if [ "$SELF_TEST" = "1" ]; then
  run_self_tests
  exit $?
fi

if [ -z "$CONFIG_ID" ]; then
  echo "missing --config-id <hyperdrive-id>" >&2
  exit 2
fi

if [ -z "${CLOUDFLARE_API_TOKEN:-}" ] || [ -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
  if [ "$ALLOW_SKIP" = "1" ]; then
    echo "check_hyperdrive_cache: no Cloudflare credentials; skipping (--allow-skip)."
    exit 0
  fi
  echo "check_hyperdrive_cache: FAIL: no Cloudflare credentials (export CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID) or pass --allow-skip for secret-less CI." >&2
  exit 3
fi

resp="$(mktemp)"
trap 'rm -f "$resp"' EXIT
code="$(curl -sS -o "$resp" -w '%{http_code}' \
  -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  -H 'Content-Type: application/json' \
  "https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/hyperdrive/configs/$CONFIG_ID")"
if [ "$code" != "200" ]; then
  echo "check_hyperdrive_cache: Hyperdrive GET failed (http $code) for $CONFIG_ID" >&2
  head -c 500 "$resp" >&2 || true
  echo >&2
  exit 1
fi

disabled="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("result",{}).get("caching",{}).get("disabled", False))' "$resp")"
if [ "$disabled" != "True" ]; then
  echo "check_hyperdrive_cache: FAIL: Hyperdrive config $CONFIG_ID must have query caching disabled." >&2
  echo "Run: wrangler hyperdrive update $CONFIG_ID --caching-disabled" >&2
  exit 1
fi
echo "check_hyperdrive_cache: ok: $CONFIG_ID has caching.disabled=true."
