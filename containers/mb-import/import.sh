#!/usr/bin/env bash
# mbdump dumps -> Postgres COPY into nntunev2-mb (idempotent, resumable).
# Usage: DATABASE_URL=<url> [DUMP_SEQ=<n>] [MB_IMPORT_CHUNK_LINES=<n>] ./import.sh <dump-dir>
# Never echo $DATABASE_URL; it arrives only via env/secret store and is
# translated into libpq PG* environment variables (pg-env.sh), so psql
# is invoked with no connection string in its argv.
#
# Kill-safe chunking: the not-yet-loaded tail of each content-addressed
# dump file is streamed once into fixed-size part files (default 2000
# lines), then each part applies in one transaction. The committed line
# number is recorded per (table, dump hash), so kill -9 resumes the same
# dump while a newer dump starts at line 0. The upsert carries a
# forward-only updated_at guard (older dumps never roll a row back).
# Contract: every row of a dump file carries that dump's single
# updated_at (the ETL stamps one DUMP_TS); the track prune compares
# per-release maxima and relies on that uniformity. The prune runs
# only in the track dump's final chunk transaction, atomically with
# the cursor advancing to the end of the dump, so an interrupted
# import leaves rows at worst stale, never truncated.

# Poison rows never abort the import: the mb-validate pre-pass checks every
# line against the typed upsert contract (uuid/int/timestamp shapes, COPY
# escapes, canonical dump layout) and poison lines land in mb_import_dlq
# with reason + raw line for the operator. FK orphans join them instead of
# aborting the chunk: a release/recording/track whose parent MBID was
# poisoned or never loaded would violate REFERENCES inside the transaction
# (whole chunk rolls back, cursor pinned, row never DLQ'd), so orphan rows
# are detected up front and filed under fk_missing_artist /
# fk_missing_release / fk_missing_recording.
# Duplicate valid MBIDs within a dump are rejected before import; the same
# MBID across distinct dumps is expected and is handled by the forward-only
# upsert. FK order is artist -> release -> recording -> track: every chunk of a
# later table only references rows an earlier table already committed, and
# immediate FK checks never see a half-chunk (a failed chunk rolls
# back whole).
#
# DUMP_SEQ (optional): replication sequence the dump corresponds to. When
# set, mb_import_state.last_packet is seeded forward-only (only when
# DUMP_SEQ > current last_packet) so the hourly replicate.sh gate
# (PACKET_SEQ = last_packet + 1) accepts the first real packet. The seed
# never touches last_applied_at/lag_seconds (apply-step columns) and an
# older dump never rewinds the cursor. When unset, state is left untouched
# and the operator must seed it before replication (see README).
#
# Input contract (curated TSVs, NOT raw MusicBrainz dumps):
#   <dump-dir>/mbdump-artist.tsv     mb_artist    (mbid, name, sort_name, type, country, updated_at)
#   <dump-dir>/mbdump-release.tsv    mb_release   (mbid, artist_mbid, title, release_type, year, caa_url, updated_at)
#   <dump-dir>/mbdump-recording.tsv  mb_recording (mbid, release_mbid, title, track_no, duration_ms, updated_at)
#   <dump-dir>/mbdump-track.tsv      mb_track     (release_mbid, recording_mbid, medium_no, track_no, updated_at)
# Produce them from an official mbdump/ extract (mbdump/artist, mbdump/release,
# mbdump/recording with the full upstream column layout) by projecting the
# curated columns in dump order, e.g. with a small ETL step that SELECTs the
# columns above and writes tab-separated rows. Missing input is fatal: a
# silent skip would report "import: done" with zero rows loaded.
set -euo pipefail

if [[ "${1:-}" == "--help" || $# -lt 1 ]]; then
  cat <<'USAGE'
Usage: DATABASE_URL=<postgres-url> ./import.sh <dump-dir>

Imports curated mbdump TSV dumps (artist, release, recording, track) in kill-safe
chunks: validate -> COPY -> typed upsert -> cursor advance, one transaction
per chunk, poison rows to mb_import_dlq. A rerun resumes from
mb_import_cursor and inserts zero dupes. See the header comment for the
curated TSV contract and how to project it from official mbdump/ extracts.
Requires: psql, python3, sha256sum and mb-validate on PATH
(MB_VALIDATE_BIN overrides), DATABASE_URL in env (never as argv).
USAGE
  exit 0
fi

# Pin the published target once. Later ETL publications may replace the
# supplied symlink, but every check/hash/read in this run must use this target.
# Published snapshots must remain immutable and retained while importing.
dump_dir="$(CDPATH='' cd -- "$1" && pwd -P)"
: "${DATABASE_URL:?set DATABASE_URL via secret store, never on the command line}"

VALIDATE_BIN="${MB_VALIDATE_BIN:-mb-validate}"
CHUNK_LINES="${MB_IMPORT_CHUNK_LINES:-2000}"
if [[ ! "$CHUNK_LINES" =~ ^[0-9]+$ ]] || (( 10#$CHUNK_LINES < 1 )); then
  echo "import: refusing MB_IMPORT_CHUNK_LINES='$CHUNK_LINES' (positive integer required)" >&2
  exit 1
fi
if [[ "$VALIDATE_BIN" == */* ]]; then
  [[ -x "$VALIDATE_BIN" ]] || { echo "import: mb-validate not executable at $VALIDATE_BIN" >&2; exit 1; }
else
  command -v "$VALIDATE_BIN" >/dev/null || { echo "import: mb-validate not on PATH (set MB_VALIDATE_BIN)" >&2; exit 1; }
fi
command -v psql >/dev/null || { echo "import: psql not on PATH" >&2; exit 1; }
command -v awk >/dev/null || { echo "import: awk not on PATH" >&2; exit 1; }
command -v tail >/dev/null || { echo "import: tail not on PATH" >&2; exit 1; }
command -v split >/dev/null || { echo "import: split not on PATH" >&2; exit 1; }
command -v sort >/dev/null || { echo "import: sort not on PATH" >&2; exit 1; }
command -v uniq >/dev/null || { echo "import: uniq not on PATH" >&2; exit 1; }
command -v sha256sum >/dev/null || { echo "import: sha256sum not on PATH" >&2; exit 1; }
command -v python3 >/dev/null || { echo "import: python3 not on PATH (FK orphan split)" >&2; exit 1; }

# Admission precedes every database mutation, including cursor creation.
# Do not reject row-level poison here: validation and FK routing stay per chunk.
for table in artist release recording track; do
  tsv="$dump_dir/mbdump-$table.tsv"
  if [[ ! -f "$tsv" || ! -r "$tsv" || ! -s "$tsv" ]]; then
    echo "import: refusing $tsv (required curated TSV must be present, readable and nonempty)" >&2
    exit 1
  fi
done

if [[ -n "${DUMP_SEQ:-}" ]]; then
  if [[ ! "$DUMP_SEQ" =~ ^[0-9]+$ ]]; then
    echo "import: refusing non-integer DUMP_SEQ '$DUMP_SEQ'" >&2
    exit 1
  fi
  # Strip arbitrary zero padding before checking length. Never evaluate long
  # input as Bash arithmetic (it can wrap); at most ten digits reach arithmetic.
  DUMP_SEQ="${DUMP_SEQ#"${DUMP_SEQ%%[!0]*}"}"
  DUMP_SEQ="${DUMP_SEQ:-0}"
  if (( ${#DUMP_SEQ} > 10 )) || (( 10#$DUMP_SEQ > 2147483647 )); then
    echo "import: refusing DUMP_SEQ outside PostgreSQL INTEGER range (0..2147483647)" >&2
    exit 1
  fi
fi

# Deliver the connection to psql via the PG* environment, never argv.
script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=pg-env.sh
. "$script_dir/pg-env.sh"
configure_pg_env_from_database_url

# Chunk scratch lives here for the whole run (one rm at the end). A kill -9
# skips the EXIT trap and leaks this directory; chunk files are disposable
# (the cursor is the resume state, never these files) and the platform GCs
# TMPDIR, so a leak is untracked clutter, never corruption.
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

refuse_all_poison_dump() {
  local table="$1" dump_id="$2" total="$3" poison_n
  poison_n="$(psql -v ON_ERROR_STOP=1 -tAX -c "SELECT count(*) FROM mb_import_dlq WHERE table_name = '$table' AND dump_id = '$dump_id';")"
  if [[ ! "$poison_n" =~ ^[0-9]+$ ]]; then
    echo "import: unexpected poison count '$poison_n' for $table dump $dump_id" >&2
    exit 1
  fi
  if (( 10#$poison_n == 10#$total )); then
    echo "import: refusing $table dump with no valid rows; replication state remains unchanged" >&2
    exit 1
  fi
}

for table in artist release recording track; do
  tsv="$dump_dir/mbdump-$table.tsv"
  prune_sql=""
  final_prune_sql=""
  case "$table" in
    artist)
      columns="mbid, name, sort_name, \"type\", country, updated_at"
      upsert="name = EXCLUDED.name, sort_name = EXCLUDED.sort_name, \"type\" = EXCLUDED.\"type\", country = EXCLUDED.country, updated_at = EXCLUDED.updated_at"
      conflict="(mbid)"
      ;;
    release)
      columns="mbid, artist_mbid, title, release_type, year, caa_url, updated_at"
      upsert="artist_mbid = EXCLUDED.artist_mbid, title = EXCLUDED.title, release_type = EXCLUDED.release_type, year = EXCLUDED.year, caa_url = EXCLUDED.caa_url, updated_at = EXCLUDED.updated_at"
      conflict="(mbid)"
      ;;
    recording)
      columns="mbid, release_mbid, title, track_no, duration_ms, updated_at"
      upsert="release_mbid = EXCLUDED.release_mbid, title = EXCLUDED.title, track_no = EXCLUDED.track_no, duration_ms = EXCLUDED.duration_ms, updated_at = EXCLUDED.updated_at"
      conflict="(mbid)"
      ;;
    track)
      columns="release_mbid, recording_mbid, medium_no, track_no, updated_at"
      # The key is the position: a track repointed at a different
      # recording in a newer dump replaces the old row instead of
      # sitting beside it at the same medium/track.
      upsert="recording_mbid = EXCLUDED.recording_mbid, updated_at = EXCLUDED.updated_at"
      conflict="(release_mbid, medium_no, track_no)"
      # Prune superseded rows, but ONLY in the final chunk's
      # transaction (selected per part below): every row of one dump
      # carries that dump's updated_at, so once the last chunk is in,
      # each release keeps only its newest-dump rows. That deletes
      # dropped/renumbered positions a newer dump no longer lists,
      # and also deletes the rows an older dump just inserted (the
      # upsert guard blocks its updates, but a fresh insert has no
      # conflict to guard, so without this it would resurrect tracks
      # the newer dump dropped). Running it in an earlier chunk would
      # delete a straddling release's not-yet-reloaded positions, so
      # a kill/failure between chunks would leave a truncated (but
      # cacheable, ingest-persistable) listing until the resume;
      # deferred to the last chunk, the prune and the cursor reaching
      # the end of the dump commit atomically and an interrupted
      # import leaves rows at worst stale, never truncated. The form
      # is release-wide (no stage restriction): by the final chunk
      # every release this dump lists is fully loaded, and a release
      # absent from the dump keeps all its rows at its own max. A
      # position whose new row landed in the DLQ (poison/FK orphan)
      # loses its old row too: a visible gap, never a stale recording.
      final_prune_sql="DELETE FROM mb_track old WHERE old.updated_at < (SELECT max(t.updated_at) FROM mb_track t WHERE t.release_mbid = old.release_mbid);"
      ;;
  esac
  # awk NR counts a trailing partial line; the validator counts the same
  # way, so cursor line numbers and part-file ranges agree on every shape.
  total="$(awk 'END { print NR+0 }' "$tsv")"
  if (( 10#$total == 0 )); then
    echo "import: refusing empty $table dump; replication state remains unchanged" >&2
    exit 1
  fi
  dump_id="$(sha256sum "$tsv" | cut -d ' ' -f 1)"
  psql -v ON_ERROR_STOP=1 -c "INSERT INTO mb_import_cursor (table_name, dump_id, lines_done) VALUES ('$table', '$dump_id', 0) ON CONFLICT (table_name, dump_id) DO NOTHING;" >/dev/null
  # Duplicate-key pre-check. Entity tables key on the MBID in column 1;
  # track keys on the position tuple (release, medium, track), where
  # column 1 legitimately repeats per track of a release. Keys are
  # COPY-decoded and canonicalized the way the validator normalizes
  # them (lowercase UUIDs, plain integers), so an octal/hex-escaped
  # alias for an already-listed key cannot slip past as a distinct key
  # for the same row. Lines whose key fields cannot validate
  # (undecodable escapes, NUL, non-UTF-8, ill-shaped keys) are skipped
  # here: the validator poisons them, and poison rows never reach the
  # table, so they cannot collide with a loaded key.
  if ! python3 - "$table" "$tsv" <<'EOF' | sort -T "$work" >"$work/mbids.sorted"; then
import sys
table, path = sys.argv[1], sys.argv[2]
NAMED = {ord("b"): 8, ord("f"): 12, ord("n"): 10, ord("r"): 13,
         ord("t"): 9, ord("v"): 11, ord("\\"): 92}
HEX = b"0123456789abcdefABCDEF"
def decode(field):
    """COPY text decode mirroring mb-import-validate; None will poison."""
    out = bytearray()
    i, n = 0, len(field)
    while i < n:
        b = field[i]
        if b != 92:
            if b == 13:
                return None
            out.append(b)
            i += 1
            continue
        i += 1
        if i >= n:
            return None
        c = field[i]
        if c in NAMED:
            out.append(NAMED[c])
            i += 1
        elif 48 <= c <= 55:
            j = i
            while j < n and j - i < 3 and 48 <= field[j] <= 55:
                j += 1
            if int(field[i:j], 8) > 255:
                return None
            out.append(int(field[i:j], 8))
            i = j
        elif c == 120:
            j = i + 1
            while j < n and j - i - 1 < 2 and field[j] in HEX:
                j += 1
            if j == i + 1:
                return None
            out.append(int(field[i + 1:j], 16))
            i = j
        else:
            return None
    if 0 in out:
        return None
    try:
        return bytes(out).decode("utf-8")
    except UnicodeDecodeError:
        return None
def uuid_key(token):
    parts = token.split("-")
    if [len(p) for p in parts] != [8, 4, 4, 4, 12]:
        return None
    if any(c not in "0123456789abcdefABCDEF" for p in parts for c in p):
        return None
    return token.lower()
def position_key(token):
    """Canonical medium/track position, or None when it cannot validate.

    Mirrors FieldKind::Position (non-negative i16): the validator poisons
    anything else, and poison rows never reach the table, so only loadable
    positions participate in duplicate detection. Keep in step with the
    schema if the position type ever widens.
    """
    digits = token[1:] if token[:1] in ("+", "-") else token
    if not digits or any(c not in "0123456789" for c in digits):
        return None
    try:
        n = int(token)
    except ValueError:
        return None
    if n < 0 or n > 32767:
        return None
    return str(n)
batch = []
def flush():
    if batch:
        sys.stdout.write("".join(key + "\n" for key in batch))
        batch.clear()
with open(path, "rb") as f:
    # Stream line by line with bounded batches: multi-million-line dumps
    # must never sit wholly in memory beside their decoded key list.
    for raw in f:
        # One trailing newline ends the line; it is not key content (awk
        # reads the same way), while a missing one still yields the tail.
        if raw.endswith(b"\n"):
            raw = raw[:-1]
        if table == "track":
            fields = raw.split(b"\t")
            if len(fields) < 4:
                continue
            release = decode(fields[0])
            medium = decode(fields[2])
            number = decode(fields[3])
            if release is None or medium is None or number is None:
                continue
            release = uuid_key(release)
            medium = position_key(medium)
            number = position_key(number)
            if release is None or medium is None or number is None:
                continue
            batch.append(release + "/" + medium + "/" + number)
        else:
            first = decode(raw.split(b"\t", 1)[0])
            if first is None:
                continue
            key = uuid_key(first)
            if key is None:
                continue
            batch.append(key)
        if len(batch) >= 8192:
            flush()
flush()
EOF
    echo "import: duplicate-key pre-check failed for $table ($tsv)" >&2
    exit 1
  fi
  uniq -d "$work/mbids.sorted" >"$work/mbids.duplicates"
  if [[ -s "$work/mbids.duplicates" ]]; then
    echo "import: duplicate key(s) in $tsv (dump contradicts itself):" >&2
    head -n 5 "$work/mbids.duplicates" >&2
    exit 1
  fi
  completed="$(psql -v ON_ERROR_STOP=1 -tAX -c "SELECT lines_done FROM mb_import_cursor WHERE table_name = '$table' AND dump_id = '$dump_id';")"
  if [[ ! "$completed" =~ ^[0-9]+$ ]]; then
    echo "import: unexpected lines_done '$completed' for $table in mb_import_cursor (migration 0014_mb_import_resume_dlq missing?)" >&2
    exit 1
  fi
  if (( 10#$completed > 10#$total )); then
    echo "import: cursor for $table is at line $completed but $tsv has $total lines; the dump shrank or was replaced mid-run: refusing (reset mb_import_cursor deliberately, never automatically)" >&2
    exit 1
  fi
  if (( 10#$completed == 10#$total )); then
    refuse_all_poison_dump "$table" "$dump_id" "$total"
    echo "import: $table already complete ($total/$total lines); skipping"
    continue
  fi
  echo "import: loading $table from $tsv (resuming at line $((10#$completed + 1)) of $total)"
  # One linear pass over the not-yet-loaded tail: stream it into fixed-size
  # part files. (A per-chunk `sed -n start,endp` re-scans the dump from line
  # 1 for every chunk: O(chunks x filesize) on multi-million-line mbdumps.)
  # Part k covers dump lines [start, end]; the resume offsets derive from
  # the part index, never from re-reading the dump.
  parts="$work/parts-$table"
  mkdir -p "$parts"
  tail -n "+$((10#$completed + 1))" "$tsv" | split -l "$CHUNK_LINES" -d -a 6 - "$parts/part-"
  shopt -s nullglob
  part_files=( "$parts"/part-* )
  shopt -u nullglob
  if (( ${#part_files[@]} == 0 )); then
    echo "import: split produced no chunks for $table (dump changed under the import?): refusing" >&2
    exit 1
  fi
  start="$((10#$completed + 1))"
  for part in "${part_files[@]}"; do
    lines="$(awk 'END { print NR+0 }' "$part")"
    if (( 10#$lines < 1 )); then
      echo "import: empty part file for $table lines starting at $start (dump changed under the import?): refusing" >&2
      exit 1
    fi
    end="$((10#$start + 10#$lines - 1))"
    offset="$((10#$start - 1))"
    range="$start-$end"
    # The validator exits 2 on any output write/flush failure, so a short
    # valid.tsv can never print counts the cursor would advance on. The
    # explicit status check keeps that fatal even under `set -e` quirks, and
    # the file recount below catches any summary/file disagreement.
    if summary="$("$VALIDATE_BIN" --table "$table" --in "$part" --valid-out "$work/valid.tsv" --poison-out "$work/poison.tsv" --line-offset "$offset")"; then
      :
    else
      validator_status=$?
      echo "import: validator failed for $table lines $range (exit $validator_status); cursor stays at $offset" >&2
      exit 1
    fi
    valid_n="${summary#valid=}"
    valid_n="${valid_n%% *}"
    poison_n="${summary##*poison=}"
    if [[ ! "$valid_n" =~ ^[0-9]+$ || ! "$poison_n" =~ ^[0-9]+$ ]]; then
      echo "import: cannot parse validator summary '$summary' for $table lines $range" >&2
      exit 1
    fi
    valid_lines="$(awk 'END { print NR+0 }' "$work/valid.tsv")"
    poison_lines="$(awk 'END { print NR+0 }' "$work/poison.tsv")"
    if [[ "$valid_lines" != "$valid_n" || "$poison_lines" != "$poison_n" ]]; then
      echo "import: validator files disagree with its summary for $table lines $range (files $valid_lines/$poison_lines, summary $valid_n/$poison_n): refusing" >&2
      exit 1
    fi
    if (( 10#$valid_n + 10#$poison_n != 10#$lines )); then
      echo "import: chunk $table lines $range yielded $((10#$valid_n + 10#$poison_n)) of $lines lines; the dump changed under the import: refusing" >&2
      exit 1
    fi
    if [[ "$table" == "track" ]]; then
      dupes="$(awk -F'\t' '{ print tolower($1) "/" ($3+0) "/" ($4+0) }' "$work/valid.tsv" | sort | uniq -d | head -n 5)"
    else
      dupes="$(awk -F'\t' '{ print $1 }' "$work/valid.tsv" | sort | uniq -d | head -n 5)"
    fi
    if [[ -n "$dupes" ]]; then
      echo "import: duplicate key inside $table chunk lines $range (dump contradicts itself; fix the dump, cursor stays at $offset):" >&2
      echo "$dupes" >&2
      exit 1
    fi
    # FK orphans never abort the chunk (see header comment): a release or
    # recording whose parent MBID was poisoned or never loaded would hit
    # REFERENCES inside the transaction, rolling back the whole chunk with
    # the cursor pinned and the row never DLQ'd. Detect those rows up front
    # against the already-committed parent table and file them under
    # fk_missing_artist / fk_missing_release / fk_missing_recording with
    # their exact dump bytes.
    # (artist has no parent; release parents commit before any release chunk
    # runs and recording parents before any recording chunk, so a missing
    # parent is poison, never chunk ordering. track has two parents and is
    # split once per parent; a row moved by the first split is already out
    # of valid.tsv for the second.)
    if (( 10#$valid_n > 0 )) && [[ "$table" != "artist" ]]; then
      case "$table" in
        release) fk_specs=("2 mb_artist fk_missing_artist") ;;
        recording) fk_specs=("2 mb_release fk_missing_release") ;;
        track) fk_specs=("1 mb_release fk_missing_release" "2 mb_recording fk_missing_recording") ;;
      esac
      for fk_spec in "${fk_specs[@]}"; do
      read -r fk_col parent_table fk_reason <<<"$fk_spec"
      (( 10#$valid_n > 0 )) || break
      # Parent MBID is column $fk_col of the normalized valid rows; `\N` is
      # NULL (no parent) and always passes. Trailing column is line_no.
      awk -F'\t' -v c="$fk_col" '$c != "\\N" { print $c "\t" $NF }' "$work/valid.tsv" >"$work/parents.tsv"
      # No explicit transaction here, so no ON COMMIT DROP: each statement
      # autocommits, which would drop the temp table before COPY runs. The
      # table dies with the session instead.
      fk_bad="$(psql -q -v ON_ERROR_STOP=1 -tAX <<SQL
CREATE TEMP TABLE want_parent (parent_mbid uuid, line_no bigint);
\copy want_parent FROM '$work/parents.tsv' WITH (FORMAT text)
SELECT coalesce(string_agg(line_no::text, ' ' ORDER BY line_no), '') FROM want_parent w WHERE NOT EXISTS (SELECT 1 FROM ${parent_table} p WHERE p.mbid = w.parent_mbid);
SQL
)"
      if [[ -n "$fk_bad" ]]; then
        # Move orphan rows out of valid.tsv, appending their exact dump
        # bytes (from the part file) escaped for the poison COPY load — the
        # same escaping the validator uses (backslash, tab, CR, LF).
        # NOTE: $fk_bad intentionally word-splits into one argv per orphan line_no.
        if ! fk_moved="$(FK_REASON="$fk_reason" FK_OFFSET="$offset" python3 - "$work/valid.tsv" "$work/poison.tsv" "$part" $fk_bad <<'EOF'
import os, sys
valid_path, poison_path, part_path = sys.argv[1], sys.argv[2], sys.argv[3]
bad = sys.argv[4:]
reason = os.environ["FK_REASON"]
offset = int(os.environ["FK_OFFSET"])
with open(part_path, "rb") as f:
    raw_lines = f.read().split(b"\n")
if raw_lines and raw_lines[-1] == b"":
    raw_lines.pop()
def esc(raw: str) -> str:
    return raw.replace("\\", "\\\\").replace("\t", "\\t").replace("\r", "\\r").replace("\n", "\\n")
kept = []
moved = 0
with open(valid_path, "r", encoding="utf-8") as f:
    for line in f:
        fields = line.rstrip("\n").rsplit("\t", 1)
        line_no = fields[1]
        if line_no in bad:
            idx = int(line_no) - offset - 1
            if idx < 0 or idx >= len(raw_lines):
                sys.stderr.write(f"fk-split: line_no {line_no} outside part range\n")
                sys.exit(1)
            kept_raw = raw_lines[idx].decode("utf-8")
            # Same CRLF rule as the validator (strip one trailing CR) so
            # FK-poison raw lines match validator-poison ones byte for byte.
            if kept_raw.endswith("\r"):
                kept_raw = kept_raw[:-1]
            with open(poison_path, "a", encoding="utf-8") as p:
                p.write(f"{reason}\t{line_no}\t{esc(kept_raw)}\n")
            moved += 1
        else:
            kept.append(line)
if moved != len(bad):
    sys.stderr.write(f"fk-split: found {moved} of {len(bad)} orphan lines in valid.tsv\n")
    sys.exit(1)
with open(valid_path, "w", encoding="utf-8") as f:
    f.writelines(kept)
print(f"moved={moved}")
EOF
)"; then
          echo "import: FK orphan split failed for $table lines $range; cursor stays at $offset" >&2
          exit 1
        fi
        valid_n="$(awk 'END { print NR+0 }' "$work/valid.tsv")"
        poison_n="$(awk 'END { print NR+0 }' "$work/poison.tsv")"
        echo "import: $table lines $range: $fk_moved orphan row(s) reference missing parents ($fk_reason), moved to DLQ"
      fi
      done
    fi
    # Single psql session + single transaction: the TEMP stage tables are
    # session-scoped, so create, COPY, upsert, DLQ load and cursor advance
    # must share one connection. The WHERE guard makes reruns move rows
    # forward only: an older dump never rolls back a row that replication
    # (or a newer dump) moved ahead. The cursor advance is conditional on
    # the part's expected offset ($offset = lines before this part, for this
    # dump hash): a concurrent importer moving the cursor first makes
    # RETURNING come back empty and this run stops instead of double-
    # applying. DLQ inserts ignore conflicts so a replayed chunk (kill
    # between COMMIT and cursor read) never double-logs poison.
    # -q keeps stdout to just the RETURNING value: without it every command
    # tag (BEGIN/COPY/INSERT/...) lands in the capture and the cursor check
    # below misfires.
    # The track prune runs only in the part that ends at the dump's last
    # line, so it commits atomically with the cursor completing the dump.
    prune_sql=""
    if (( 10#$end == 10#$total )); then
      prune_sql="$final_prune_sql"
    fi
    advanced="$(psql -q -v ON_ERROR_STOP=1 -tAX <<SQL
BEGIN;
CREATE TEMP TABLE stage (LIKE mb_${table}) ON COMMIT DROP;
\copy stage ($columns, line_no) FROM '$work/valid.tsv' WITH (FORMAT text)
INSERT INTO mb_${table} ($columns, line_no) SELECT $columns, line_no FROM stage
ON CONFLICT $conflict DO UPDATE SET $upsert
WHERE mb_${table}.updated_at < EXCLUDED.updated_at;
$prune_sql
CREATE TEMP TABLE poison (reason text, line_no bigint, raw text) ON COMMIT DROP;
\copy poison FROM '$work/poison.tsv' WITH (FORMAT text)
INSERT INTO mb_import_dlq (table_name, dump_id, line_no, reason, raw_line)
SELECT '$table', '$dump_id', line_no, reason, raw FROM poison
ON CONFLICT (table_name, dump_id, line_no) DO NOTHING;
UPDATE mb_import_cursor SET lines_done = $end, updated_at = now()
WHERE table_name = '$table' AND dump_id = '$dump_id' AND lines_done = $offset
RETURNING lines_done;
COMMIT;
SQL
)"
    if [[ "$advanced" != "$end" ]]; then
      echo "import: cursor for $table moved under this run (expected $end, got '$advanced'); stopping without damage" >&2
      exit 1
    fi
    echo "import: $table lines $start-$end committed ($valid_n rows, $poison_n poison)"
    start="$((10#$end + 1))"
  done
  refuse_all_poison_dump "$table" "$dump_id" "$total"
done

# Record the dump's replication sequence so replicate.sh accepts the first
# real packet. Forward-only seed: advances last_packet only when
# DUMP_SEQ > current last_packet, and never touches last_applied_at /
# lag_seconds (those are written only by the replicate apply step). An
# older-dump rerun (DUMP_SEQ <= last_packet) warns and leaves state
# untouched so the cursor never rewinds. Unset DUMP_SEQ leaves state
# untouched (fresh DBs stay at the migration seed 0); the operator then
# seeds last_packet before replication.
# DUMP_SEQ was normalized and range-checked before the first write.
if [[ -n "${DUMP_SEQ:-}" ]]; then
  current_last_packet="$(psql -v ON_ERROR_STOP=1 -tAX -c "SELECT last_packet FROM mb_import_state WHERE id = 1;")"
  if [[ ! "$current_last_packet" =~ ^[0-9]+$ ]]; then
    echo "import: unexpected last_packet '$current_last_packet' in mb_import_state" >&2
    exit 1
  fi
  if (( 10#$DUMP_SEQ <= 10#$current_last_packet )); then
    echo "import: DUMP_SEQ=$DUMP_SEQ <= current last_packet=$current_last_packet; leaving state untouched (older dump never rewinds the replication cursor)" >&2
  else
    psql -v ON_ERROR_STOP=1 -c "UPDATE mb_import_state SET last_packet = $DUMP_SEQ, updated_at = now() WHERE id = 1 AND last_packet < $DUMP_SEQ;"
    echo "import: seeded mb_import_state.last_packet=$DUMP_SEQ"
  fi
else
  echo "import: DUMP_SEQ unset; leaving mb_import_state untouched (seed last_packet before running replicate.sh)" >&2
fi

echo "import: done"
