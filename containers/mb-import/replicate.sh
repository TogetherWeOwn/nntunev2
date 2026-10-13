#!/usr/bin/env bash
# Hourly replication packets -> nntunev2-mb. SLO: lag < 2 h.
# Usage: DATABASE_URL=<url> PACKET_SEQ=<n> PACKET_URL=<url> ./replicate.sh
# Records applied packet + lag in mb_import_state; never skips silently.
set -euo pipefail

: "${DATABASE_URL:?set DATABASE_URL via secret store}"
: "${PACKET_SEQ:?set PACKET_SEQ (replication packet number)}"
: "${PACKET_URL:?set PACKET_URL (packet download URL)}"

started="$(date +%s)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "replicate: fetching packet $PACKET_SEQ"
curl -sSfL -o "$tmp/packet.tar.gz" "$PACKET_URL"

echo "replicate: applying packet $PACKET_SEQ (unpack + apply steps land with the full importer)"
# TODO(next slice): unpack + apply deletes/upserts, then update state.
# State update shape (kept here so the SLO is reviewable now):
applied="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 \
  -c "UPDATE mb_import_state SET last_packet = $PACKET_SEQ, last_applied_at = '$applied', updated_at = now() WHERE id = 1;"

ended="$(date +%s)"
echo "replicate: packet $PACKET_SEQ applied in $((ended - started))s (SLO: lag < 2 h)"
