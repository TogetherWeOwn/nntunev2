# Database migrations

Every SQL migration is recorded once in `manifest.toml`, in strictly increasing numeric order. Use the next four-digit number after the current highest number for each new file. The gate requires unique, increasing numbers but does not require existing migration sequences to be gap-free.
(Sources: `migrations/manifest.toml` — one entry per migration, strictly increasing; `.github/scripts/migrations_gate.py` `check_manifest` (`duplicate-entry`, `duplicate-number`, `out-of-order` for non-increasing numbers) and `check_drift` (new numbers must continue `base_max + 1, +2, …`); enforced in CI by the `migrations-gate` job in `.github/workflows/ci.yml`.)

Add a manifest entry for every new migration with its explicit `target` (`app` or `mirror`) and the lowercase SHA-256 checksum of the file contents. The CI migration gate validates the manifest, numbering, targets, and checksums.
(Sources: `migrations/manifest.toml` — every `[[migration]]` carries `file`, `target` (`app` or `mirror`) and a lowercase-hex `sha256`; `.github/scripts/migrations_gate.py` `check_manifest` (`bad-target`, `checksum` requiring `[0-9a-f]{64}` equal to the file bytes, `unlisted`, `missing-file`).)

Never edit or rename an applied migration. Migrations are forward-only: correct a deployed schema with a new migration, not by changing history.
(Source: `.github/scripts/migrations_gate.py` `check_drift` — `applied-edited` and `applied-removed`.)

Replay-safety: `workers/api/tests/run_migration_tests.sh` applies every `migrations/*.sql` in order to an empty database, then re-applies the full set to prove each migration is a clean no-op on replay. Two migrations are excluded from the replay pass: `0006_account_deletion.sql`, which predates the replay check and carries an unconditional `ADD COLUMN`, and `0021_billing_schema.sql`, whose seed names columns that `0026_plan_reseed_no_trial.sql` drops; applied migrations cannot be edited, so both are intentionally excluded.
(Source: `workers/api/tests/run_migration_tests.sh` — the apply loop reapplies every migration except `0006_account_deletion.sql` and `0021_billing_schema.sql`, with the exclusion reasons recorded inline.)

`search_path` convention: every new migration is self-contained under any role or runner `search_path`.

The first statement of every new migration numbered above `0030` is exactly `SET search_path = public, pg_catalog;` (`public` first so new objects land there; no `$user`, no other writable schema). Leading comments and blank lines may precede it; nothing executable may. The CI migration gate enforces this for files new against the base ref (`missing-search-path`).
(Source: `.github/scripts/migrations_gate.py` `check_drift` — a new number above 0030 whose file does not open with the exact `SET` header; enforced in CI by the `migrations-gate` job in `.github/workflows/ci.yml`.)

Schema-qualify every relation reference in new migrations: `CREATE/ALTER/DROP TABLE public.…`, `CREATE INDEX … ON public.…`, `REFERENCES public.…`, `'…'::regclass` literals, and seed `INSERT INTO public.…`. (Review-enforced: the gate cannot parse SQL.)
Extensions use `CREATE EXTENSION … WITH SCHEMA public`, so the extension lands in `public` regardless of the caller's `search_path` (operator-class resolution, e.g. `gin_trgm_ops` in `0001`, also follows `search_path`, which qualification alone cannot fix).
Any future function carries a function-level `SET search_path = public, pg_catalog` attribute (mandatory for `SECURITY DEFINER`).

Migrations that predate this convention are manifest-pinned and immutable, so they carry no header — including `0030_mb_mirror_w6b.sql`, which must never be edited to add one (that breaks the manifest checksum pin for zero gain over the session pin below). `workers/api/tests/run_migration_tests.sh` applies every migration with the session pinned (`PGOPTIONS='-c search_path=public,pg_catalog'` in `psql_run`); that wrapper is the required way to apply any pre-convention file in every environment. Do not rely on `ALTER ROLE/DATABASE … SET search_path`.
(Source: `workers/api/tests/run_migration_tests.sh` `psql_run`; `.github/scripts/migrations_gate.py` `check_drift` — the header check covers only files new against the base ref, so already-merged files are unaffected.)
