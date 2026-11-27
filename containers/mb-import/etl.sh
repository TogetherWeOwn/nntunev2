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
if ! IFS= read -r -d '' dump_dir < <(CDPATH='' cd -P -- "$1" 2>/dev/null && printf '%s\0' "$PWD"); then
  printf 'etl: input directory does not exist or is inaccessible: %s\n' "$1" >&2
  exit 1
fi
exec python3 "$script_dir/etl.py" "$dump_dir"
