# mb-import-validate

Curated mbdump TSV row validator for the mb-import container job. Per
physical dump line it decides whether the line is safe to feed to Postgres
`COPY (FORMAT text)` plus the typed upsert, and if not, which poison reason
routes it to the dead-letter table. It never touches the database:
`containers/mb-import/import.sh` drives it as the `mb-validate` binary.

(Sources: scope `src/lib.rs:1-7`; driven by `import.sh`
`containers/mb-import/import.sh:78,385`.)

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

(Sources: contracts `src/lib.rs:9-13`; field counts `src/lib.rs:45-54`;
job-side input contract `containers/mb-import/import.sh:45-49` and per-table
columns `containers/mb-import/import.sh:155,160,165,170`.)

Per-column kinds (`src/lib.rs:101-134`): `artist` is
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
(`src/lib.rs:83-98`). Checks run in this order per field: verbatim `\N`
gate, COPY-escape decode, empty-value rule, then the per-kind shape check
on the decoded value (`src/lib.rs:197-297`).

- **Shape:** wrong field count is `column_count`; an empty line is
  `empty_line` (`src/lib.rs:92-98`). Typed columns validate their
  COPY-decoded value: UUIDs must decode to lowercase-or-uppercase hex
  `8-4-4-4-12` with no braces or `urn:` (`bad_uuid`; see
  `src/lib.rs:405-416`). Smallints must decode to an `i16` (`bad_int`,
  `src/lib.rs:254-256`); durations to a non-negative `i32`
  (`negative_duration`, `src/lib.rs:279-282`); medium/track positions to a
  non-negative `i16` (`negative_position`, `src/lib.rs:287-290`).
  Timestamps must decode to `YYYY-MM-DD`, optionally followed by `T` or
  space plus `HH:MM:SS[.f{1,6}]`; `Z`/`z` may follow either form, while a
  `±hh`, `±hhmm` or `±hh:mm` offset (hours ≤ 15, minutes ≤ 59) is accepted
  only after a time — `HH:MM` without seconds, a date-only value with a
  numeric offset, and >6 fractional digits are `bad_timestamp`. Impossible
  months/days and leap-day on non-leap years are also rejected
  (`is_timestamp`, `src/lib.rs:420-553`; seconds required
  `src/lib.rs:514-521,543`; numeric zone only after a time
  `src/lib.rs:452-458`). Country must decode to
  exactly 2 chars (`bad_country`, `src/lib.rs:274-277`): `\x55S` is `US`,
  while the two-byte spelling `\t` is one character and poison.
- **Escapes:** one bounded decoder validates and decodes every field:
  only the documented COPY text escapes may follow a backslash (`\\`,
  `\b \f \n \r \t \v`, octal `\` + 1-3 octal digits, hex `\x` + 1-2 hex
  digits); anything else — including `\q`, `\o`, `\u`, a bare `\x`,
  `\8`/`\9`, out-of-range `\400`, NUL-producing and non-UTF8-decoding
  sequences, or a raw CR — is `bad_escape`
  (`src/lib.rs:320-381`). Deliberately stricter than COPY, which would load
  unknown escapes mangled instead of refusing them.
- **NULL discipline:** a literal `\N` in a NOT NULL column is
  `null_in_not_null` poison (it would load as NULL and violate the
  constraint inside the chunk transaction); in a nullable column it passes
  through as the NULL marker (`src/lib.rs:206-211`). The gate sees the raw
  token only: an ENCODED backslash-N (e.g. `\134N`) is data, never NULL.
  An empty NOT NULL
  timestamp/uuid/numeric is `bad_timestamp` / `bad_uuid` / `bad_int`
  (an empty value would be rejected by COPY into the typed staging table
  — `""` fails for timestamptz and numerics — and abort the chunk
  transaction; the staging table carries no column defaults
  (`CREATE TEMP TABLE stage (LIKE mb_${table})`,
  `containers/mb-import/import.sh:528`), so no `now()` stamping occurs);
  an empty NOT NULL text column
  passes through as `""` (`src/lib.rs:222-239`).

Poison reason strings are lowercase snake_case with no tabs and are copied
verbatim into the dead-letter table (`src/lib.rs:67-81`):
`column_count`, `empty_line`, `bad_uuid`, `bad_int`, `bad_timestamp`,
`bad_escape`, `null_in_not_null`, `bad_country`, `negative_duration`,
`negative_position`, `invalid_encoding`. `invalid_encoding` is produced only
by the binary for non-UTF8 input bytes (`src/main.rs:123-138`); the
`fk_missing_*` reasons are produced only by `import.sh`'s orphan split, not
by this crate (`containers/mb-import/import.sh:431-506`).

## Normalization and `line_no` (`\N` + append rule, pinned to source)

- **`\N` normalization:** an empty value in a **nullable** column of any
  kind normalizes to the COPY null marker `\N` (`src/lib.rs:238`); a
  literal `\N` already in a nullable column stays `\N`
  (`src/lib.rs:206-211`).
- **Typed canonicalization:** every spelling of one typed value emits one
  canonical COPY-safe form, so encoded/literal aliases load the same key:
  UUIDs emit lowercase (`src/lib.rs:246-252`); smallints, durations and
  positions emit plain integers (`+001`, `0002`, `\x31` all emit `1`;
  `src/lib.rs:253-256,278-282,286-290`); timestamps emit their decoded
  value, which a validated timestamp keeps COPY-safe
  (`src/lib.rs:257-265`); countries emit their decoded value re-escaped
  for COPY (`\x55S` emits `US`, a decoded tab emits `\t`, a decoded
  backslash-N emits `\\N` and never becomes NULL;
  `src/lib.rs:274-277`, escaper `src/lib.rs:388-402`).
- **Text pass-through:** text columns keep the original token
  byte-equal (`src/lib.rs:295`); Postgres decodes COPY text escapes on
  import, so the stored value is the decoded meaning of the dump's
  encoded bytes (`src/lib.rs:20-22`). Poison rows always keep the exact
  original dump bytes, never normalized values.
- **`line_no` append:** the 1-based line number in the whole dump file —
  chunk offset added by the caller — is appended as the final column, and
  the loader COPYs it into `line_no` (`src/lib.rs:19-20,85-88,141-142`).
  The binary computes it as `--line-offset + index + 1`
  (`src/main.rs:117-118`).

`escape_poison_field` escapes a poison raw line (`reason \t line_no \t
raw`) so it round-trips through COPY text: backslash first, then tab/CR/LF
and NUL (`src/lib.rs:145-160`).

## Supported input contract

Short version of the sections above, for dump producers:

| Input spelling | Typed meaning | Emitted to COPY |
|---|---|---|
| Literal canonical (`US`, `2024`, `1`, lowercase UUID, `2024-02-29 12:34:56+00:00`) | as written | byte-equal |
| Supported COPY escapes in a typed field (`\141`, `\x55`, `\303\251`, `\040`, `\t`, `\\`) | decoded value | canonical re-escaped form |
| Uppercase UUID, padded/signed integers (`+001`, `0002`) | same value | lowercase UUID / plain integer |
| Empty or `\N` in a nullable column | NULL | `\N` |
| Decoded backslash-N in a typed field (`\134N`) | two data chars | `\\N` (never NULL) |

Refused, always: malformed/unsupported escapes (`\q`, `\o`, `\u`,
bare `\x`, `\8`), out-of-range octal (`\400`), NUL-producing and
non-UTF-8 decodings, raw CR/NUL, semantic failures on the decoded value
(overflow, negative duration/position, impossible dates) under the same
reason codes as their literal twins. Text columns accept the same escape
vocabulary but pass the token through untouched. The importer's
whole-file and per-chunk duplicate checks decode and canonicalize keys
the same way before comparing, so aliases cannot evade refusal
(`containers/mb-import/import.sh:208-340,409-418`).

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
  disagreement (`containers/mb-import/import.sh:385-408`). Every output
  write and flush is checked: a short write (disk full, closed pipe) exits
  2 instead of printing counts the cursor would advance on
  (`src/main.rs:119-172`).
- **Plan mode:**
  `mb-validate --plan --completed <n> --total <n> --size <n>` prints the
  1-based inclusive chunk ranges still pending after the resume cursor, one
  `start-end` per line (`src/main.rs:8-9,45-56`, ranges
  `src/lib.rs:165-180`). `--size 0` is a usage error; a closed downstream
  (BrokenPipe) is a clean shutdown (`src/main.rs:49-72`).
- **Exit codes:** 0 success, 2 usage or I/O error (`src/main.rs:11`); any
  validator exit other than 0 keeps the cursor pinned
  (`containers/mb-import/import.sh:385-390`).
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
