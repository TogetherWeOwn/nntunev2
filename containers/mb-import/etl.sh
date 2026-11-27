#!/usr/bin/env bash
set -euo pipefail

if [[ $# -eq 1 && ( "$1" == --help || "$1" == -h ) ]]; then
  echo 'Usage: DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>'
  exit 0
fi
if [[ $# -ne 1 || -z "$1" || "$1" == -* ]]; then
  echo 'Usage: DUMP_TS=<ISO-8601 timestamp> ./etl.sh <mbdump-dir>' >&2
  exit 2
fi

script_dir="$(CDPATH='' cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
IFS= read -r -d '' dump_dir < <(CDPATH='' cd -P -- "$1" && printf '%s\0' "$PWD")
exec python3 "$script_dir/etl.py" "$dump_dir"
