#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
container_dir="$(cd -- "$script_dir/.." && pwd)"
repo_dir="$(cd -- "$container_dir/../.." && pwd)"
temp_dir="$(mktemp -d)"
trap 'rm -rf "$temp_dir"' EXIT

# Use the real native validator, never a mock or a widened timestamp parser.
if [[ -n "${MB_VALIDATE_BIN:-}" ]]; then
  validate_bin="$MB_VALIDATE_BIN"
else
  host="$(rustc -vV | python3 -c 'import sys; print(next(line.split(": ", 1)[1].strip() for line in sys.stdin if line.startswith("host:")))')"
  cargo build --quiet --locked --offline --manifest-path "$repo_dir/Cargo.toml" \
    -p mb-import-validate --bin mb-validate --target "$host"
  target_dir="$(cargo metadata --locked --offline --no-deps --format-version 1 \
    --manifest-path "$repo_dir/Cargo.toml" | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  validate_bin="$target_dir/$host/debug/mb-validate"
fi

python3 - "$container_dir/etl.sh" "$script_dir/fixtures/mbdump" "$validate_bin" "$temp_dir" <<'PY'
import os
from pathlib import Path
import shutil
import subprocess
import sys
from datetime import datetime, timezone

etl, fixtures, validator, scratch = map(Path, sys.argv[1:])
validator = validator.resolve()
counts = {"artist": 2, "release": 2, "recording": 2, "track": 4}
failures = []


def check(condition, message):
    if not condition:
        failures.append(message)
        print(f"FAIL: {message}", file=sys.stderr)


def run_etl(raw, timestamp):
    env = dict(os.environ)
    if timestamp is None:
        env.pop("DUMP_TS", None)
    else:
        env["DUMP_TS"] = timestamp
    return subprocess.run(["bash", str(etl), str(raw)], env=env,
                          capture_output=True, text=True, check=False)


def publication(raw):
    # Include symlinks and every snapshot file so failed input cannot replace
    # the current publication or silently create an orphaned snapshot.
    state = {}
    for path in sorted(raw.iterdir()):
        if path.name != "curated" and not path.name.startswith(".curated-"):
            continue
        if path.is_symlink():
            state[path.name] = ("symlink", os.readlink(path))
        elif path.is_dir():
            state[path.name] = ("directory",)
            for child in sorted(path.rglob("*")):
                if child.is_file():
                    state[str(child.relative_to(raw))] = ("file", child.read_bytes())
        else:
            state[path.name] = ("file", path.read_bytes())
    return state


# UTC expectations are literal, hand-checked instants, not ETL-derived values.
# Different importer-supported serializations of the same instant are allowed.
cases = [
    ("lowercase_t", "2026-10-18t12:00:00Z", "2026-10-18T12:00:00+00:00"),
    ("uppercase_t", "2026-10-18T12:00:00Z", "2026-10-18T12:00:00+00:00"),
    ("positive_offset", "2026-10-18T14:30:00+02:30", "2026-10-18T12:00:00+00:00"),
    ("negative_fractional", "2026-10-18T07:00:00.123456-05:00", "2026-10-18T12:00:00.123456+00:00"),
    ("fractional_utc", "2026-10-18T12:00:00.5Z", "2026-10-18T12:00:00.500000+00:00"),
    ("space", "2026-10-18 12:00:00+00:00", "2026-10-18T12:00:00+00:00"),
    ("alternate_separator", "2026-10-18_12:00:00Z", "2026-10-18T12:00:00+00:00"),
    ("omitted_seconds", "2026-10-18T12:00Z", "2026-10-18T12:00:00+00:00"),
    ("large_offset", "2026-10-19T04:00:00+16:00", "2026-10-18T12:00:00+00:00"),
    ("offset_seconds", "2026-10-18T12:00:30+00:00:30", "2026-10-18T12:00:00+00:00"),
]
for name, timestamp, expected_utc in cases:
    before_failures = len(failures)
    raw = scratch / name
    shutil.copytree(fixtures, raw)
    result = run_etl(raw, timestamp)
    check(result.returncode == 0,
          f"{name}: timezone-aware DUMP_TS was refused: {result.stderr.strip()}")
    if result.returncode != 0:
        continue
    curated = raw / "curated"
    check(curated.is_dir(), f"{name}: ETL succeeded without a publication")
    if not curated.is_dir():
        continue
    for table, expected_count in counts.items():
        source = curated / f"mbdump-{table}.tsv"
        rows = source.read_text().splitlines()
        check(len(rows) == expected_count,
              f"{name}/{table}: expected {expected_count} curated rows, got {len(rows)}")
        for number, row in enumerate(rows, 1):
            emitted = row.split("\t")[-1]
            try:
                parsed = datetime.fromisoformat(emitted.replace("Z", "+00:00"))
                same_instant = (parsed.utcoffset() is not None and
                                parsed.astimezone(timezone.utc) == datetime.fromisoformat(expected_utc))
            except ValueError:
                same_instant = False
            check(same_instant,
                  f"{name}/{table}:{number}: timestamp {emitted!r} lost instant {expected_utc}")
        valid = scratch / f"{name}-{table}.valid.tsv"
        poison = scratch / f"{name}-{table}.poison.tsv"
        validated = subprocess.run([
            str(validator), "--table", table, "--in", str(source),
            "--valid-out", str(valid), "--poison-out", str(poison),
            "--line-offset", "0",
        ], capture_output=True, text=True, check=False)
        check(validated.returncode == 0,
              f"{name}/{table}: validator execution failed: {validated.stderr.strip()}")
        if validated.returncode != 0:
            continue
        reasons = [row.split("\t", 1)[0] for row in poison.read_text().splitlines()]
        # Quarantine is exit 0: the summary, actual valid rows and poison output
        # must agree, rather than treating successful execution as acceptance.
        check(validated.stdout.strip() == f"valid={expected_count} poison=0" and
              len(valid.read_text().splitlines()) == expected_count and not poison.read_bytes(),
              f"{name}/{table}: accepted ETL timestamp {timestamp!r} must feed through "
              f"without poison; got {validated.stdout.strip()!r}, reasons={reasons}")
    first = publication(raw)
    repeated = run_etl(raw, timestamp)
    check(repeated.returncode == 0, f"{name}: identical rerun failed: {repeated.stderr.strip()}")
    check(publication(raw) == first,
          f"{name}: identical input/timestamp changed snapshot target or bytes")
    if len(failures) == before_failures:
        print(f"PASS: {name}: all four tables accepted; instant and deterministic publication preserved")

invalid = [
    ("unset", None),
    ("empty", ""),
    ("malformed", "not-a-timestamp"),
    ("naive_uppercase", "2026-10-18T12:00:00"),
    ("naive_lowercase", "2026-10-18t12:00:00"),
    ("date_only", "2026-10-18"),
    ("impossible_date", "2026-02-30T12:00:00Z"),
    ("invalid_hour", "2026-10-18T25:00:00Z"),
    ("invalid_offset", "2026-10-18T12:00:00+25:00"),
]
for name, timestamp in invalid:
    before_failures = len(failures)
    raw = scratch / f"invalid-{name}"
    shutil.copytree(fixtures, raw)
    result = run_etl(raw, timestamp)
    check(result.returncode != 0, f"{name}: invalid/missing-zone DUMP_TS was accepted")
    check(not publication(raw), f"{name}: refused fresh input left a publication")
    seeded = run_etl(raw, "2026-10-18T12:00:00Z")
    check(seeded.returncode == 0 and (raw / "curated").is_dir(),
          f"{name}: could not seed prior snapshot: {seeded.stderr.strip()}")
    if seeded.returncode != 0 or not (raw / "curated").is_dir():
        continue
    first = publication(raw)
    result = run_etl(raw, timestamp)
    check(result.returncode != 0, f"{name}: invalid rerun was accepted")
    check(publication(raw) == first,
          f"{name}: refusal changed prior snapshot target, bytes or directory set")
    if len(failures) == before_failures:
        print(f"PASS: {name}: refused before publication; prior snapshot unchanged")

if failures:
    raise AssertionError(f"ETL timestamp contract: {len(failures)} failing assertions")
print(f"mb-import ETL timestamp contract: PASS ({len(cases)} accepted, {len(invalid)} refused cases)")
PY
