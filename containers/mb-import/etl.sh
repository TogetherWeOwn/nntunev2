#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "--help" ]]; then
  echo 'Usage: DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>'
  exit 0
fi
if [[ $# -ne 1 ]]; then
  echo 'Usage: DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>' >&2
  exit 2
fi

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/etl.py" "$1"
