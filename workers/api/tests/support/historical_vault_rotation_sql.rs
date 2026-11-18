//! Pre-purge SQLite fixture SQL for the legacy provider vault.
//! The claim and CAS match the historical SQL in the KEK runbook §6.
//! This is compiled only into the re-wrap integration tests.

pub const CLAIM_BATCH_SQL: &str = "SELECT user_id, kek_version, dek_wrapped, dek_wrapped_nonce
  FROM provider_vaults
 WHERE kek_version < $1 AND user_id > $2
 ORDER BY user_id LIMIT $3;";

pub const CAS_REWRAP_SQL: &str = "UPDATE provider_vaults
      SET kek_version = $new, dek_wrapped = $w,
          dek_wrapped_nonce = $n, updated_at = $now,
          rewrapped_at = $now
    WHERE user_id = $u AND kek_version = $old
      AND dek_wrapped = $captured_wrapped
      AND dek_wrapped_nonce = $captured_wrapped_nonce;";
