# nntune-config

Typed accessor over the versioned KV config schema. New Worker code must read
config keys through `Config`, never with raw stringly KV lookups on config
paths; existing Workers still read env/KV directly (e.g.
`workers/api/src/worker_entry.rs`) and migrate incrementally, so the guarantees
below apply once a caller uses `Config::load`.
Compiles for native targets and `wasm32-unknown-unknown`.

The schema source of truth is [config/schema.json](../../config/schema.json):
every key declares its type, default, and env-overridability. This crate pins
the accessor side of that contract. The schema-side procedure (adding a key,
fixtures, parity checker, CI job) is owned by the
[config dir README](../../config/README.md) and is referenced, not repeated,
here.

Implementation: [src/lib.rs](src/lib.rs).

## Contract

- **Load validates all.** `Config::load` validates the whole key set at once:
  undeclared keys fail, defaults fill unset keys, required keys without a
  default must be present, and every value must parse as its declared type.
  Parse rules: numbers and bools tolerate surrounding whitespace; strings are
  kept exact but empty/whitespace-only strings are rejected; bools accept only
  `true`/`false` (case-insensitive).
  (Sources: `src/lib.rs:56-85` for parsing, `src/lib.rs:262-287` for load;
  pinned by `defaults_apply_for_unset_keys`,
  `missing_required_key_without_default_fails`, `invalid_types_rejected`,
  `unknown_kv_key_rejected` in `src/lib.rs:356-427`, and
  `empty_and_blank_strings_rejected` in `src/lib.rs:481-517` for the
  empty/whitespace-string rejection matching the CI parity checker.)
- **Env overlay is scoped and atomic.** `Config::apply_env` consults only keys
  declared `env_overridable`, and only through their documented `env_var`
  name; every other environment entry is ignored. Precedence is KV first,
  environment second (local dev and `wrangler [vars]` win). The overlay is
  atomic: every override is parsed first and the config is only mutated when
  all of them are valid, so a failing overlay leaves previous values
  untouched (fail-closed).
  (Sources: `src/lib.rs:289-309`; pinned by
  `env_overlay_only_touches_overridable_keys`,
  `env_overlay_type_errors_fail_closed`, and
  `env_overlay_is_atomic_across_vars` in `src/lib.rs:430-478`.)
- **Never secrets.** Secret values are never config keys: Worker secrets and
  rotation live outside KV config entirely. Error text reports key names
  only, never values, so it is safe for CI logs. This holds by construction
  — the error variants carry key names only (`src/lib.rs:229-236`); no test
  asserts the `Display` output.
  (Sources: `src/lib.rs:11-12` for the rule, `src/lib.rs:226-251` for the
  fail-closed error type.)
- **Schema version bumps with meaning changes.** `SCHEMA_VERSION` is the
  version of `config/schema.json` this accessor implements; bump it together
  with the schema file — the cross-check test fails on drift. Bump
  `schema_version` when you change the meaning of an existing key, not when
  you add one; a supported version bump updates the schema file,
  `SCHEMA_VERSION`, the accepted version in `validate_schema` in
  `../../.github/scripts/config_parity_check.py:119`, and its self-test (which
  must then reject the next unsupported version, currently pinned at
  `../../.github/scripts/test_config_parity_check.py:113` rejecting version
  2). Adding a key follows the procedure in the
  [config dir README](../../config/README.md) (schema file, both env
  fixtures, and the `KEYS` table together).
  (Sources: `src/lib.rs:19-21`, `src/lib.rs:88-91`; pinned by
  `schema_file_matches_code_table` in `src/lib.rs:558-638`, which also checks
  key set, types, required flags, env-overridability, env vars, and defaults.)

## Reading values

Typed getters: `get_u64` / `get_bool` / `get_str`. `None` means the key is
absent (only possible for non-required keys without a default) or held under
another type, which cannot happen after `Config::load` validated the set.
(Sources: `src/lib.rs:311-333`; pinned by
`typed_getters_return_none_for_absent_or_other_typed_keys` in
`src/lib.rs:550-555`. Bool parsing/getter coverage is kept via a synthetic
spec while the shipped schema has no bool key — see
`synthetic_bool_key_parses_and_is_read_by_get_bool` in `src/lib.rs:520-547`;
that spec must not become a production key.)

The shipped staging/prod fixtures load through this same typed path (see
`shipped_fixtures_load_through_typed_getters` and
`shipped_fixture_mutations_fail_closed` in `src/lib.rs:663-792`); fixture and
parity rules live in the [config dir README](../../config/README.md).

## Checks

```sh
cargo test -p nntune-config
```
