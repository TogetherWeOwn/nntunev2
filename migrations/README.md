# Database migrations

Every SQL migration is recorded once in `manifest.toml`, in strictly increasing numeric order. Use the next four-digit number after the current highest number for each new file. The gate requires unique, increasing numbers but does not require existing migration sequences to be gap-free.
(Sources: `migrations/manifest.toml` — one entry per migration, strictly increasing; `.github/scripts/migrations_gate.py` `check_manifest` (`duplicate-entry`, `duplicate-number`, `out-of-order` for non-increasing numbers) and `check_drift` (new numbers must continue `base_max + 1, +2, …`); enforced in CI by the `migrations-gate` job in `.github/workflows/ci.yml`.)

Add a manifest entry for every new migration with its explicit `target` (`app` or `mirror`) and the lowercase SHA-256 checksum of the file contents. The CI migration gate validates the manifest, numbering, targets, and checksums.
(Sources: `migrations/manifest.toml` — every `[[migration]]` carries `file`, `target` (`app` or `mirror`) and a lowercase-hex `sha256`; `.github/scripts/migrations_gate.py` `check_manifest` (`bad-target`, `checksum` requiring `[0-9a-f]{64}` equal to the file bytes, `unlisted`, `missing-file`).)

Never edit or rename an applied migration. Migrations are forward-only: correct a deployed schema with a new migration, not by changing history.
(Source: `.github/scripts/migrations_gate.py` `check_drift` — `applied-edited` and `applied-removed`.)

Applied migrations are recorded in each database's `public.schema_migrations` table (`file`, `sha256`, `applied_at`, `applied_by`), written by `.github/scripts/apply_migration.py` in the same transaction as the apply. The staging deploy gate compares those rows against this manifest, so a migration that was applied but not recorded (or recorded but never applied) fails the deploy.
(Sources: `.github/scripts/apply_migration.py` `compose` and `DDL`; `.github/scripts/staging_api_schema_gate.py` `load_expected` and `compare`; `docs/ops.md` "Staging migrations".)

A file renumbered on a PR branch after staging already applied it must be renamed byte-identically (filename only — not even the header comment changes) and keeps its original name, with the entry's own sha, in its manifest entry as `renamed_from = [{ file = "…", sha256 = "…" }]`; the database row keeps the name it was applied under and the gate recognises it through this history. Recording `renamed_from` is metadata, never an edit of the applied migration.
(Sources: `.github/scripts/migrations_gate.py` `load_manifest` and the `bad-rename` checks in `check_manifest`; `.github/scripts/migrations_rename_check.py` `check`, run by the `migrations-staging` workflow.)

Replay-safety: `workers/api/tests/run_migration_tests.sh` applies every `migrations/*.sql` in order to an empty database, then re-applies the full set to prove each migration is a clean no-op on replay. Two migrations are excluded from the replay pass: `0006_account_deletion.sql`, which predates the replay check and carries an unconditional `ADD COLUMN`, and `0021_billing_schema.sql`, whose seed names columns that `0026_plan_reseed_no_trial.sql` drops; applied migrations cannot be edited, so both are intentionally excluded.
(Source: `workers/api/tests/run_migration_tests.sh` — the apply loop reapplies every migration except `0006_account_deletion.sql` and `0021_billing_schema.sql`, with the exclusion reasons recorded inline.)
