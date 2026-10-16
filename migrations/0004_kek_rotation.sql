-- NNTune v2 migration 0004: KEK re-wrap follow-up (docs/security/kek-rotation.md §2/§8).
--
-- PlanetScale Postgres via Hyperdrive. Adds the authoritative re-wrap
-- marker and the version index the re-wrap pass (§6) scans by. Before this
-- migration, re-wrap passes emulated the marker via `updated_at`; from here
-- on the conditional re-wrap UPDATE sets `rewrapped_at = $now` alongside
-- `kek_version`, and `rewrapped_at IS NULL` means "never re-wrapped" (the
-- row still sits at its insert-time KEK version). Ciphertext semantics are
-- untouched: no column here holds plaintext, and existing rows need no
-- backfill — NULL is the correct value for every pre-migration row.
--
-- `idx_vault_kek_version` serves the §6 batch claim
-- (`WHERE kek_version < $1 ...`) and the per-version progress counts
-- documented in §6, without a sequential scan of the vault table.

ALTER TABLE provider_vaults ADD COLUMN rewrapped_at BIGINT NULL;
CREATE INDEX IF NOT EXISTS idx_vault_kek_version ON provider_vaults (kek_version);
