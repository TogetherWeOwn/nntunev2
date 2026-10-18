# mb-import-validate

Curated mbdump TSV row validator for the mb-import container job. Per
physical dump line it decides whether the line is safe to feed to Postgres
`COPY (FORMAT text)` plus the typed upsert, and if not, which poison reason
routes it to the dead-letter table. It never touches the database:
`containers/mb-import/import.sh` drives it as the `mb-validate` binary.

(Sources: scope `src/lib.rs:1-7`; driven by `import.sh`
`containers/mb-import/import.sh:66,75,246`.)

Container-job prose (chunking, cursor, upsert, prune, resume) lives with the
job itself; this README pins only this crate's API and the `mb-validate`
binary contract.

## Curated column contracts

Each curated TSV carries exactly these columns, in dump order; anything else
is `column_count` poison. The field counts are enforced per table.

| Table (`--table`) | Columns | Fields |
|---|---|---|
| `artist` | mbid, name, sort_name, type, country, updated_at | 6 |
| `release` | mbid, artist_mbid, title, release_type, year, caa_url, updated_at | 7 |
| `recording` | mbid, release_mbid, title, track_no, duration_ms, updated_at | 6 |
| `track` | release_mbid, recording_mbid, medium_no, track_no, updated_at | 5 |

(Sources: contracts `src/lib.rs:9-13`; field counts `src/lib.rs:43-50`;
job-side input contract `containers/mb-import/import.sh:45-49` and per-table
columns `containers/mb-import/import.sh:131,136,141,146`.)

Per-column kinds (`src/lib.rs:97-130`): `artist` is
uuid/not-null, text/not-null, text/not-null, text/nullable,
country/nullable, timestamp/not-null; `release` is uuid/not-null,
uuid/nullable, text/not-null, text/nullable, smallint/nullable,
text/nullable, timestamp/not-null; `recording` is uuid/not-null,
uuid/nullable, text/not-null, smallint/nullable, duration/nullable,
timestamp/not-null; `track` is uuid/not-null, uuid/not-null,
position/not-null, position/not-null, timestamp/not-null.

## What makes a line poison

`validate_line(table, line_no, raw)` returns the normalized line on success
or `(reason, line_no)` for the dead-letter table on failure
(`src/lib.rs:79-94`). Checks run in this order per field: verbatim `\N`
gate, COPY-escape check, empty-value rule, then the per-kind shape check
(`src/lib.rs:190-271`).

- **Shape:** wrong field count is `column_count`; an empty line is
  `empty_line` (`src/lib.rs:88-94`). UUIDs must be lowercase-or-uppercase
  hex `8-4-4-4-12` with no braces or `urn:` (`bad_uuid`,
  `src/lib.rs:353-364`). Smallints must parse as `i16` (`bad_int`,
  `src/lib.rs:235-241`); durations as non-negative `i32`
  (`negative_duration`, `src/lib.rs:256-260`); medium/track positions as
  non-negative `i16` (`negative_position`, `src/lib.rs:261-268`).
  Timestamps are `YYYY-MM-DD`, optionally followed by `T` or space plus
  `HH:MM:SS[.f{1,6}]`; `Z`/`z` may follow either form, while a `±hh`,
  `±hhmm` or `±hh:mm` offset (hours ≤ 15, minutes ≤ 59) is accepted only
  after a time — `HH:MM` without seconds, a date-only value with a numeric
  offset, and >6 fractional digits are `bad_timestamp`. Impossible
  months/days and leap-day on non-leap years are also rejected
  (`bad_timestamp`, `src/lib.rs:366-501`; seconds required
  `src/lib.rs:462-472,495`; numeric zone only after a time
  `src/lib.rs:403-409`). Country must be
  empty or exactly 2 chars (`bad_country`, `src/lib.rs:249-255`).
- **Escapes:** only the documented COPY text escapes may follow a
  backslash (`\\`, `\b \f \n \r \t \v`, octal `\` + 1-3 octal digits, hex
  `\x` + 1-2 hex digits); anything else — including `\q`, `\o`, `\u`, a
  bare `\x`, `\8`/`\9`, out-of-range `\400`, NUL-producing and
  non-UTF8-decoding sequences, or a raw CR — is `bad_escape`
  (`src/lib.rs:274-351`). Deliberately stricter than COPY, which would load
  unknown escapes mangled instead of refusing them.
- **NULL discipline:** a literal `\N` in a NOT NULL column is
  `null_in_not_null` poison (it would load as NULL and violate the
  constraint inside the chunk transaction); in a nullable column it passes
  through as the NULL marker (`src/lib.rs:190-201`). An empty NOT NULL
  timestamp/uuid/numeric is `bad_timestamp` / `bad_uuid` / `bad_int`
  (an empty value would be rejected by COPY into the typed staging table
  — `""` fails for timestamptz and numerics — and abort the chunk
  transaction; the staging table carries no column defaults
  (`CREATE TEMP TABLE stage (LIKE mb_${table})`,
  `containers/mb-import/import.sh:389`), so no `now()` stamping occurs);
  an empty NOT NULL text column
  passes through as `""` (`src/lib.rs:209-226`).

Poison reason strings are lowercase snake_case with no tabs and are copied
verbatim into the dead-letter table (`src/lib.rs:63-77`):
`column_count`, `empty_line`, `bad_uuid`, `bad_int`, `bad_timestamp`,
`bad_escape`, `null_in_not_null`, `bad_country`, `negative_duration`,
`negative_position`, `invalid_encoding`. `invalid_encoding` is produced only
by the binary for non-UTF8 input bytes (`src/main.rs:123-138`); the
`fk_missing_*` reasons are produced only by `import.sh`'s orphan split, not
by this crate (`containers/mb-import/import.sh:292-296`).

## Normalization and `line_no` (`\N` + append rule, pinned to source)

- **`\N` normalization:** an empty value in a **nullable** column of any
  kind normalizes to the COPY null marker `\N` (`src/lib.rs:225`); a
  literal `\N` already in a nullable column stays `\N`
  (`src/lib.rs:196-201`). (The crate doc comment's "nullable
  numeric/uuid/timestamp" phrasing at `src/lib.rs:15-16` is shorthand: the
  code applies the rule to every nullable kind, including text and
  country.) The validator preserves other field bytes in its COPY output
  (`src/lib.rs:17-19`); Postgres then decodes COPY text escapes on import,
  so the stored value is the decoded meaning of the dump's encoded bytes.
- **`line_no` append:** the 1-based line number in the whole dump file —
  chunk offset added by the caller — is appended as the final column, and
  the loader COPYs it into `line_no` (`src/lib.rs:16-17,81-84,137-138`).
  The binary computes it as `--line-offset + index + 1`
  (`src/main.rs:117-118`).

`escape_poison_field` escapes a poison raw line (`reason \t line_no \t
raw`) so it round-trips through COPY text: backslash first, then tab/CR/LF
and NUL (`src/lib.rs:141-156`).

## `mb-validate` binary role

Two modes; poison rows are expected data and never change the exit code
(`src/main.rs:1-11`).

- **Validate mode** (default):
  `mb-validate --table artist|release|recording|track --in <tsv>
  --valid-out <tsv> --poison-out <tsv> --line-offset <n>`
  (`src/main.rs:17-22,74-81`). Writes normalized valid rows and
  `reason \t line_no \t escaped-raw` poison rows, then prints exactly
  `valid=<n> poison=<m>` (`src/main.rs:144-161,173`). `import.sh` parses
  that summary, recounts both files against it, and refuses on any
  disagreement (`containers/mb-import/import.sh:246-265`). Every output
  write and flush is checked: a short write (disk full, closed pipe) exits
  2 instead of printing counts the cursor would advance on
  (`src/main.rs:119-172`).
- **Plan mode:**
  `mb-validate --plan --completed <n> --total <n> --size <n>` prints the
  1-based inclusive chunk ranges still pending after the resume cursor, one
  `start-end` per line (`src/main.rs:8-9,45-56`, ranges
  `src/lib.rs:158-173`). `--size 0` is a usage error; a closed downstream
  (BrokenPipe) is a clean shutdown (`src/main.rs:49-72`).
- **Exit codes:** 0 success, 2 usage or I/O error (`src/main.rs:11`); any
  validator exit other than 0 keeps the cursor pinned
  (`containers/mb-import/import.sh:242-252`).
- **Line splitting:** input splits on LF only, matching how Postgres reads
  the same file; one trailing newline ends the last line and is not an
  extra empty line, while an empty file yields zero lines
  (`src/main.rs:107-116`). One trailing CR per line is stripped so CRLF
  dumps validate like the LF dumps COPY would read
  (`src/main.rs:140-142`).

## Checks

```sh
cargo test -p mb-import-validate
cargo clippy -p mb-import-validate --all-targets
python3 .github/scripts/docs_link_lint.py --root .
python3 .github/scripts/public_text_lint.py
```
