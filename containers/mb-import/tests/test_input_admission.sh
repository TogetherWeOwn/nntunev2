#!/usr/bin/env bash
# Offline behavioral acceptance: no database, credentials or Cargo required.
set -euo pipefail
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$script_dir/input_admission/test_input_admission.py" "$@"
