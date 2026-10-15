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
#   ./check_hyperdrive_cache.sh --config-id <hyperdrive-id>
#   CLOUDFLARE_API_TOKEN=... CLOUDFLARE_ACCOUNT_ID=... ./check_hyperdrive_cache.sh --config-id <id>
#
# Skips (exit 0) when credentials are absent so fork PRs without secrets stay
# green, matching the deploy workflow gate. Fails when caching is enabled or
# the config cannot be read with credentials present.
set -euo pipefail

CONFIG_ID=""
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
    -h|--help)
      sed -n '1,20p' "$0"
      exit 0
      ;;
    *)
      echo "unknown arg: $1 (want --config-id <id>)" >&2
      exit 2
      ;;
  esac
done

if [ -z "$CONFIG_ID" ]; then
  echo "missing --config-id <hyperdrive-id>" >&2
  exit 2
fi

if [ -z "${CLOUDFLARE_API_TOKEN:-}" ] || [ -z "${CLOUDFLARE_ACCOUNT_ID:-}" ]; then
  echo "check_hyperdrive_cache: no Cloudflare credentials; skipping (fork-safe)."
  exit 0
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
