#!/usr/bin/env bash
# Retired: hourly replication packets are Live Data Feed material under the
# NC licence variant and never enter the CC0-only mirror. Product reads come
# from the twice-weekly CC0 core dumps via import.sh; see
# containers/mb-import/README.md (Jobs table, Replication row) and
# docs/adr/0007-mb-mirror.md.
# Fail closed: refuse every invocation BEFORE reading DATABASE_URL,
# PACKET_SEQ or PACKET_URL, touching the network, or writing any state.
# Exit 2 preserves the old pending-apply refusal signal for any caller that
# branches on it.
set -euo pipefail

echo "replicate: retired; replication packets are not ingested (CC0-only mirror via import.sh dumps); refusing without touching secrets, network or state" >&2
exit 2
