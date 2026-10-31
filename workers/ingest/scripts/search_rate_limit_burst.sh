#!/usr/bin/env bash
# Burst check for the Newznab search abuse guard (roadmap R6).
#
# Fires SEARCH_RATE_LIMIT_MAX+1 (default 11) identical search requests at
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
: "${INGEST_URL:?set INGEST_URL to the ingest worker base URL}"
: "${INGEST_GRANT:?set INGEST_GRANT to an ingest:write caller grant}"

for i in $(seq 1 11); do
  printf 'attempt %02d: ' "$i"
  curl -s --connect-timeout 10 -m 30 -o /dev/null -D - -w '%{http_code}\n' \
    -H 'content-type: application/json' \
    -H "authorization: Bearer $INGEST_GRANT" \
    -d '{"query":"burst probe","indexer_ref":"vault_idx_01-ab","categories":[],"limit":25,"page":0}' \
    "$INGEST_URL/v1/ingest/search" \
    | grep -iE '^[0-9]{3}$|^retry-after:' | tr '\n' ' '
  echo
done
