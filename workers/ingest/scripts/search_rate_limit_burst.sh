#!/usr/bin/env bash
# Burst check for the Newznab search abuse guard (roadmap R6).
#
# Fires 11 identical search requests for the default ten-request limit at
# a running ingest worker and prints one status line each. Expected: the
# first 10 are non-429 (202 once the queue/meter bindings accept, or 503
# while they do not — both prove the request passed the throttle), the
# 11th is 429 with a `retry-after` header. A single follow-up request as a
# different user from this host may still 429 (per-IP bucket is shared) —
# that is the dual gate working; run it after the window (60s default) to
# see a legit request pass again.
#
# Never point this script at production. Do not retry: 429/503 are observations,
# and replaying a request changes both the burst count and the throttle result.
# Every ingest route authenticates an `ingest:write` caller grant before the
# throttle, so INGEST_GRANT must be a live grant (see src/caller_auth.rs) or
# all 11 attempts stop at 401. The grant carries the user; the body has none.
# Usage: INGEST_URL=https://<worker-host> INGEST_GRANT=v1.<...> ./search_rate_limit_burst.sh
set -euo pipefail
set +x
if (( $# )); then
  echo 'burst: configure INGEST_URL and INGEST_GRANT via the environment' >&2
  exit 2
fi
: "${INGEST_URL:?set INGEST_URL to the ingest worker base URL}"
: "${INGEST_GRANT:?set INGEST_GRANT to an ingest:write caller grant}"
if [[ ! "$INGEST_URL" =~ ^(https?://)([a-zA-Z0-9]([a-zA-Z0-9.-]*[a-zA-Z0-9])?|\[::1\])(:([0-9]{1,5}))?/?$ ]]; then
  echo 'burst: INGEST_URL must be a worker origin' >&2
  exit 2
fi
scheme="${BASH_REMATCH[1]}" host="${BASH_REMATCH[2]}" port="${BASH_REMATCH[5]}"
if [[ -n "$port" ]] && (( 10#$port < 1 || 10#$port > 65535 )); then
  echo 'burst: INGEST_URL has an invalid port' >&2
  exit 2
fi
if [[ "$scheme" == http:// && "$host" != localhost && "$host" != 127.0.0.1 && "$host" != '[::1]' ]]; then
  echo 'burst: HTTPS is required for non-loopback workers' >&2
  exit 2
fi
if (( ${#INGEST_GRANT} > 505 )) || [[ ! "$INGEST_GRANT" =~ ^v1\.([a-fA-F0-9]{2})+\.[a-fA-F0-9]{64}$ ]]; then
  echo 'burst: INGEST_GRANT has an invalid format' >&2
  exit 2
fi
export -n INGEST_GRANT

for ((i = 1; i <= 11; i++)); do
  printf 'attempt %02d: ' "$i"
  printf 'authorization: Bearer %s\n' "$INGEST_GRANT" \
    | curl -q -s --globoff --proto '=http,https' --noproxy 'localhost,127.0.0.1,::1' \
    --connect-timeout 10 -m 30 -o /dev/null -D - -w '%{http_code}\n' \
    -H 'content-type: application/json' \
    -H @- \
    -d '{"query":"burst probe","indexer_ref":"vault_idx_01-ab","categories":[],"limit":25,"page":0}' \
    -- "${INGEST_URL%/}/v1/ingest/search" \
    | grep -iE '^[0-9]{3}$|^retry-after:[[:blank:]]*[0-9]+[[:space:]]*$' | tr '\r\n' ' '
  echo
done
