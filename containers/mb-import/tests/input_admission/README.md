# Input admission acceptance handoff

This is a deliberately RED, test-only handoff against baseline
`511fbab601c338d8aad2874ca622060116b600bd`. Do not merge the tests alone.
Continue the acceptance branch with the importer fix in one implementation PR.
The test author owns corrections; the Builder must not rewrite these tests to
make the implementation pass.

Protected paths:

- `containers/mb-import/tests/test_input_admission.sh`
- `containers/mb-import/tests/input_admission/` (this README and both Python files)

## Reproduce

From the repository root, as an ordinary unprivileged user:

```sh
bash containers/mb-import/tests/test_input_admission.sh
```

Requires Bash, Python 3 and the standard tools already required by `import.sh`
(awk, tail, split, sort, uniq, sha256sum, cut). No psql installation, Rust build,
container, credentials or database is needed. Only a local psql double runs;
its connection target is a synthetic `.invalid` name. An inherited
`DATABASE_URL`, `DUMP_SEQ`, `PG*` or `MB_VALIDATE_BIN` is not passed to the
importer. On a root process, the four unreadability tests explicitly skip;
use an unprivileged process for the full 40-test result.

Optional diagnostics (captures are synthetic and never contain live data):

```sh
INPUT_ADMISSION_KEEP=1 bash containers/mb-import/tests/test_input_admission.sh
# Each test prints its capture directory. Otherwise captures are removed.
bash containers/mb-import/tests/test_input_admission.sh \
  InputAdmission.test_symlink_replaced_after_artist_phase_still_imports_only_A_then_B
```

Scratch uses `PAPERCLIP_RUN_SCRATCH_DIR` / `PAPERCLIP_SCRATCH_DIR` when supplied,
otherwise Python's normal temporary directory. Each case has a separate
configuration, cursor/DLQ/sequence reply state, SQL/COPY/validator JSONL capture,
and numbered importer stdout/stderr. The psql double captures mutation attempts
before returning any simulated database error; an eventual rollback or range
error cannot hide a late admission check. Unsupported SQL is a harness error,
not an expected RED result; request corrections from the test author if the
process protocol legitimately changes.

## Baseline expectations

40 tests: **22 failures, 18 passes, no errors or skips** on an unprivileged
process. The suite must become fully green with the implementation.

| Failing group | Cases | Exact behavioral failure |
| --- | ---: | --- |
| Publication pin | 2 | First run's COPY bytes are B instead of A (release after artist completion; artist between hashing and chunk read). |
| Later-file admission | 9 | Missing/empty/unreadable release, recording, track attempt 2, 5, 8 mutations respectively, expected 0. |
| Sequence admission | 11 | Malformed/overflow sequences attempt 12 mutations, or 13 for the two integer-range cases, expected 0. |

Publication switching is synchronous: the psql double atomically replaces the
relative `curated` link when the artist completion probe runs, or immediately
after the artist cursor insertion. No sleep, polling or scheduling race is
involved. The first import must submit only A; the next import must submit B.
A has two rows per table and B has three, with different names and timestamps.
Tests independently check COPY bytes, content-addressed cursor hashes and final
line counts, not just importer log text.

Invalid sequence cases: negative, leading plus, fraction, leading/trailing
space, exponent, text, `2147483648`, `0002147483648`,
`18446744073709551616` (unsigned wrap to zero) and 200 nines. Valid controls:
unset/empty, zero on both migration seed and a newer cursor, older/equal,
forward, `2147483647`, `00042`, and 200 leading zeroes followed by `42`.

Other green controls exercise ordinary directories with spaces, same-dump
skip, independent newer-dump hashes and forward-only upsert SQL, poison and
FK-orphan COPY payloads with original raw bytes/line numbers, all-poison refusal
on initial and completed paths, and deterministic interrupted-track resume.
The interrupted case stops the second track validation, proves the first
chunk contains no prune, then proves only the tail is submitted on resume and
the final transaction includes the prune before sequence seeding.

## Evidence boundaries and existing tests

The real shell performs file checks, counting, hashing, splitting, orphan
routing and SQL generation. Python process doubles replace only psql and the
native validator. They do **not** implement PostgreSQL entity storage, FK or
upsert execution, pruning, transaction rollback or actual UUID/timestamp
validation. SQL-boundary assertions are not evidence of database correctness
or whole-import atomicity. The tiny validator recognizes only synthetic
`bad_uuid` rows; this suite does not replace native-validator tests.

Retain `containers/mb-import/test-import.sh` unchanged, including its
`ALLOW_TEST_TRUNCATE=1` fence. Its actual scratch-database checks remain the
entity/cursor/DLQ/state and interrupted-track integration contract. Run those
only through the existing approved disposable test-database route with the
native validator available, never staging or production. Any native compilation
must use the bounded `scripts/cargo_cache.py` pool and stop on refusal (75).
Do not edit applied migrations.

Focused offline neighbors:

```sh
bash -n containers/mb-import/import.sh containers/mb-import/test-import.sh \
  containers/mb-import/tests/test_input_admission.sh
bash containers/mb-import/tests/test_etl.sh
bash containers/mb-import/tests/test_etl_reference_graphs.sh
python3 containers/mb-import/tests/test_etl_timestamp_bounds.py
bash containers/mb-import/tests/test_pg_env.sh
bash containers/mb-import/tests/test_argv_hygiene.sh
```

The separate `test_etl_timestamp_contract.sh` also exercises the native
validator and therefore needs the provisioned bounded Cargo pool. No source,
existing acceptance test, migration, CI workflow or security fence is changed
by this handoff.
