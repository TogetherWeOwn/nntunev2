# KV config: typed schema + staging/prod parity

The `nntunev2-config-staging` / `nntunev2-config-prod` KV namespaces hold
public, non-secret runtime tuning (TTLs, limits, hostnames). Keys used to be
untyped and staging/prod drifted silently; this directory versions them and
makes drift fail CI.

## Layout

| File | Role |
|---|---|
| `schema.json` | Source of truth (currently `schema_version` 1). Every key declares `type` (one of `u64`, `bool`, `string`), `default` (or `null` when the key must be set per environment), `required`, `env_overridable`, and `env_var`. |
| `staging.json` / `prod.json` | The expected key sets per environment. Keys and types must match across both files; **values may differ** (hostnames and per-env tuning are the point). |
| `fixtures/` | Matching + drifted pairs consumed by the parity self-tests. `values-differ-prod.json` proves value differences stay green; `drift-*` files prove the missing-key, type-mismatch, and undeclared-key modes red (key-set asymmetry is pinned with a synthetic optional-key case in the self-tests, not a `drift-*` file). |
| `../crates/nntune-config` | Typed Rust accessor over the same key table (native + wasm32). |

## Rules

- **Never secrets.** Secret values and rotation live in Worker secrets / CI
  secrets, never in KV schema, fixtures, or logs. The accessor's error text
  reports key names only, never values.
- **Workers read config through the typed accessor.** New code paths use
  `nntune_config::Config::load` (whole-set validation: defaults applied,
  missing required keys rejected, type mismatches rejected, undeclared keys
  rejected) plus `apply_env` for the documented per-key overrides. No raw
  stringly KV lookups on config paths in new code. Ordinary tuning constants
  may migrate to the accessor incrementally; security invariants are not
  configuration knobs and remain enforced in code (for example, the vault
  indexer must never follow redirects per F-INGEST-01).
- **Precedence is KV first, environment second.** `apply_env` only consults
  keys declared `env_overridable`, and only through their documented
  `env_var` name. Everything else in the environment is ignored.

## Adding a key

1. Add the key to `schema.json` with its `type`, `default` (or `null` for
   per-environment identity like hostnames), `required`,
   `env_overridable`, and `env_var` (required exactly when overridable).
2. Add the same key to `staging.json` and `prod.json` (same type; values may
   differ), and mirror it in the `KEYS` table in
   `crates/nntune-config/src/lib.rs`.
3. Run the gates locally before pushing:
   - `python3 .github/scripts/config_parity_check.py --schema config/schema.json --staging config/staging.json --prod config/prod.json`
   - `python3 .github/scripts/test_config_parity_check.py`
   - `python3 .github/scripts/test_config_duplicate_members.py`
   - `cargo test -p nntune-config` (also covered by CI's workspace test run)
4. Read the key in workers only via `Config::get_u64` / `get_bool` /
   `get_str`. Bump `schema_version` (schema file + `SCHEMA_VERSION`) when
   you change the meaning of an existing key, not when you add one.

## CI

The `config-parity` job (part of the required `ci-ok` gate) runs the parity
check on the shipped schema and environment files, the script's self-tests, and the
duplicate-member tests. It fails on a missing required key, a type
mismatch, an undeclared key, or a staging/prod key-set asymmetry, and
passes on identical fixtures or fixtures that differ in values only.

## Sources (verified 2026-10-18)

- Namespaces: `docs/ops.md:112-113` (staging/prod ids) and `:335`
  (Config KV row).
- Schema shape: `config/schema.json:2` (version), key blocks e.g. `:5-12`,
  null-default identity keys `:37-52`. Allowed types per
  `.github/scripts/config_parity_check.py:29` (`TYPES`) and
  `crates/nntune-config/src/lib.rs:24-39` (`ConfigType`); no shipped key
  currently uses `bool` — covered synthetically in both suites.
- Parity semantics: `config_parity_check.py:171-195` (keys and types
  compared, values never compared); exit codes `:12-14`.
- Fixture roles: identity divergence in `config/staging.json:6-7` vs
  `config/prod.json:6-7`; pins in
  `.github/scripts/test_config_parity_check.py:45-108` (asymmetry
  synthetic `:87-108`).
- Accessor: `lib.rs:21` (version), `:46-53` (`KeySpec`), `:91-212`
  (`KEYS`), `:265-287` (`load`), `:296-309` (`apply_env`), `:314-333`
  (getters), `:238-251` (key-names-only errors); cross-checks `:558-638`
  and `:664-750`. `env_var`-exactly-when-overridable per
  `config_parity_check.py:144-148` and `lib.rs:43-44`.
- CI: `.github/workflows/ci.yml:391-404` (three run steps); `ci-ok` is
  required (`:3`) with `config-parity` in needs (`:753`); `cargo test
  --workspace` in `rust-test` (`:47-51`) covers `crates/nntune-config`
  (workspace member per `Cargo.toml:2`).
- Indexer redirect example: `workers/api/src/vault.rs:46-47`
  (`INDEXER_ALLOW_REDIRECTS = false`),
  `docs/security/threat-model.md:61` (F-INGEST-01 requires redirects to be
  forbidden; the fetch-policy model follows up to 3 re-validated redirects —
  unresolved discrepancy, see also `:193`); rejected-as-unknown-key pin at
  `lib.rs:417-425`.
