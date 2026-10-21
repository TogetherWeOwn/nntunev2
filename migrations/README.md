# Database migrations

Every SQL migration is recorded once in `manifest.toml`, in strictly increasing numeric order. Use the next four-digit number after the current highest number for each new file. The gate requires unique, increasing numbers but does not require existing migration sequences to be gap-free.
(Sources: `migrations/manifest.toml` — `0001`–`0021`, one entry each, strictly increasing; `.github/scripts/migrations_gate.py` `check_manifest` (`duplicate-entry`, `duplicate-number`, `out-of-order` for non-increasing numbers) and `check_drift` (new numbers must continue `base_max + 1, +2, …`); enforced in CI by the `migrations-gate` job in `.github/workflows/ci.yml`.)

Add a manifest entry for every new migration with its explicit `target` (`app` or `mirror`) and the lowercase SHA-256 checksum of the file contents. The CI migration gate validates the manifest, numbering, targets, and checksums.
(Sources: `migrations/manifest.toml` — every `[[migration]]` carries `file`, `target` (`app` or `mirror`) and a lowercase-hex `sha256`; `.github/scripts/migrations_gate.py` `check_manifest` (`bad-target`, `checksum` requiring `[0-9a-f]{64}` equal to the file bytes, `unlisted`, `missing-file`).)

Never edit or rename an applied migration. Migrations are forward-only: correct a deployed schema with a new migration, not by changing history.
(Source: `.github/scripts/migrations_gate.py` `check_drift` — `applied-edited` and `applied-removed`.)

Replay-safety: `workers/api/tests/run_migration_tests.sh` applies every `migrations/*.sql` in order to an empty database, then re-applies the full set to prove each migration is a clean no-op on replay. The single exception is the immutable pre-check migration `0006_account_deletion.sql`, which predates the replay check and carries an unconditional `ADD COLUMN`; applied migrations cannot be edited, so it is intentionally excluded from the replay pass.
(Source: `workers/api/tests/run_migration_tests.sh` — the apply loop reapplies every migration except `0006_account_deletion.sql`, with the exclusion reason recorded inline.)
