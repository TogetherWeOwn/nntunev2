//! Transactional service account registry. Lock order matches every other
//! admin write: shared serialization lock, actor row, then provider row.

use super::*;
use crate::pool_providers::{
    self, PoolProviderRow, PoolProviderSettings, PoolProviderWrite, PoolProviderWriteOutcome,
};

const ROW_COLUMNS: &str = "(to_jsonb(p) - 'secret_ref' - 'revision' - 'created_at' - 'updated_at')::text AS settings, p.secret_ref, p.revision";

fn from_row(row: &tokio_postgres::Row) -> Result<PoolProviderRow, StoreError> {
    let settings: String = row.try_get("settings").map_err(|_| StoreError)?;
    let settings: PoolProviderSettings = serde_json::from_str(&settings).map_err(|_| StoreError)?;
    settings.validate().map_err(|_| StoreError)?;
    let secret_ref: String = row.try_get("secret_ref").map_err(|_| StoreError)?;
    let revision: i64 = row.try_get("revision").map_err(|_| StoreError)?;
    if !pool_providers::lower_uuid_ok(&secret_ref) || revision <= 0 {
        return Err(StoreError);
    }
    Ok(PoolProviderRow {
        settings,
        secret_ref,
        revision: revision as u64,
    })
}

pub(super) async fn list_on(
    client: &impl GenericClient,
) -> Result<Vec<PoolProviderRow>, StoreError> {
    // Registry configuration, not a user-sized catalogue. Refuse rather than
    // silently truncate and hide a configured kill switch from an operator.
    let rows = client
        .query_typed(
            &format!("SELECT {ROW_COLUMNS} FROM pool_providers p ORDER BY p.tier, p.id LIMIT 1001"),
            &[],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() > 1000 {
        return Err(StoreError);
    }
    rows.iter().map(from_row).collect()
}

pub(super) async fn account_on(
    client: &impl GenericClient,
    provider_id: &str,
) -> Result<Option<crate::pool_resolver::StoredAccount>, StoreError> {
    if !pool_providers::lower_uuid_ok(provider_id) {
        return Err(StoreError);
    }
    let rows = client
        .query_typed(
            &format!(
                "SELECT {ROW_COLUMNS}, s.encrypted_creds, s.creds_envelope_version, \
            s.creds_kek_version, s.creds_dek_wrapped_nonce, s.creds_dek_wrapped, \
            s.creds_payload_nonce, s.creds_payload_format \
            FROM pool_providers p JOIN pool_provider_secrets s ON s.secret_ref=p.secret_ref \
            WHERE p.id=$1::uuid"
            ),
            &[(&provider_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() > 1 {
        return Err(StoreError);
    }
    rows.first()
        .map(|row| {
            let version: i16 = row
                .try_get("creds_envelope_version")
                .map_err(|_| StoreError)?;
            let format: i16 = row
                .try_get("creds_payload_format")
                .map_err(|_| StoreError)?;
            let kek_version: i32 = row.try_get("creds_kek_version").map_err(|_| StoreError)?;
            let wrapped_nonce: Vec<u8> = row
                .try_get("creds_dek_wrapped_nonce")
                .map_err(|_| StoreError)?;
            let payload_nonce: Vec<u8> =
                row.try_get("creds_payload_nonce").map_err(|_| StoreError)?;
            let dek_wrapped: Vec<u8> = row.try_get("creds_dek_wrapped").map_err(|_| StoreError)?;
            let ciphertext: Vec<u8> = row.try_get("encrypted_creds").map_err(|_| StoreError)?;
            if version != pool_providers::CREDS_ENVELOPE_VERSION
                || format != pool_providers::CREDS_PAYLOAD_FORMAT_JSON_V1
                || kek_version <= 0
                || dek_wrapped.len() != 48
                || ciphertext.len() < 16
            {
                return Err(StoreError);
            }
            Ok(crate::pool_resolver::StoredAccount {
                row: from_row(row)?,
                kek_version: kek_version as u32,
                envelope: pool_providers::PoolSecretEnvelope {
                    dek_wrapped_nonce: wrapped_nonce.try_into().map_err(|_| StoreError)?,
                    dek_wrapped,
                    payload_nonce: payload_nonce.try_into().map_err(|_| StoreError)?,
                    ciphertext,
                },
            })
        })
        .transpose()
        // Query failures above are store outages (500). Malformed stored
        // material is an unavailable account, just like a failed decrypt
        // (503); never distinguish corrupt envelope metadata on the wire.
        .or(Ok(None))
}

pub(super) async fn write_on(
    client: &mut tokio_postgres::Client,
    actor_user_id: &str,
    write: &PoolProviderWrite,
    source_ip: &str,
    now: u64,
) -> Result<PoolProviderWriteOutcome, StoreError> {
    write.validate().map_err(|_| StoreError)?;
    let now = i64_of(now)?;
    let tx = client.transaction().await.map_err(|_| StoreError)?;
    tx.query_typed(
        "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
        &[],
    )
    .await
    .map_err(|_| StoreError)?;
    let actor = tx
        .query_typed(
            "SELECT role FROM users WHERE id = $1 AND deleting_at IS NULL FOR UPDATE",
            &[(&actor_user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if actor.len() > 1 {
        return Err(StoreError);
    }
    let role: Option<String> = actor
        .first()
        .map(|row| row.try_get("role").map_err(|_| StoreError))
        .transpose()?;
    if !crate::admin::is_admin_role(role.as_deref()) {
        return Ok(PoolProviderWriteOutcome::ActorNotAdmin);
    }
    let rows = tx
        .query_typed(
            &format!("SELECT {ROW_COLUMNS} FROM pool_providers p WHERE p.id = $1::uuid FOR UPDATE"),
            &[(&write.settings.id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if rows.len() > 1 {
        return Err(StoreError);
    }
    let previous = rows.first().map(from_row).transpose()?;
    if previous.as_ref().map_or(0, |row| row.revision) != write.expected_revision {
        return Ok(PoolProviderWriteOutcome::Conflict);
    }
    let secret_ref = match (&write.secret, &previous) {
        (Some(secret), _) => secret.secret_ref.clone(),
        (None, Some(old)) => old.secret_ref.clone(),
        _ => return Err(StoreError),
    };
    if previous
        .as_ref()
        .is_some_and(|old| write.secret.is_some() && old.secret_ref == secret_ref)
    {
        return Err(StoreError);
    }
    if let Some(secret) = &write.secret {
        tx.query_typed(
            "INSERT INTO pool_provider_secrets (secret_ref, encrypted_creds, creds_envelope_version, \
             creds_kek_version, creds_dek_wrapped_nonce, creds_dek_wrapped, creds_payload_nonce, \
             creds_payload_format, created_at, updated_at) \
             VALUES ($1, $2, 2, $3, $4, $5, $6, 1, $7, $7)",
            &[
                (&secret.secret_ref, Type::TEXT),
                (&secret.envelope.ciphertext, Type::BYTEA),
                (&(secret.kek_version as i32), Type::INT4),
                (&&secret.envelope.dek_wrapped_nonce[..], Type::BYTEA),
                (&secret.envelope.dek_wrapped, Type::BYTEA),
                (&&secret.envelope.payload_nonce[..], Type::BYTEA),
                (&now, Type::INT8),
            ],
        ).await.map_err(|_| StoreError)?;
    }
    // Populate only the validated settings type; never deserialize a caller's
    // secret_ref/revision/timestamps into the database record.
    let settings = serde_json::to_string(&write.settings).map_err(|_| StoreError)?;
    let revision = (write.expected_revision + 1) as i64;
    let changed = if previous.is_none() {
        tx.query_typed(
            "INSERT INTO pool_providers (id, label, host, port, tls, username, can_read, can_post, \
             max_connections, stream_reserve, ingest_limit, backbone, tier, enabled, secret_ref, \
             revision, created_at, updated_at) \
             SELECT s.id, s.label, s.host, s.port, s.tls, s.username, s.can_read, s.can_post, \
             s.max_connections, s.stream_reserve, s.ingest_limit, s.backbone, s.tier, s.enabled, \
             $2, $3, $4, $4 FROM jsonb_populate_record(NULL::pool_providers, $1::jsonb) s \
             ON CONFLICT (id) DO NOTHING RETURNING id::text",
            &[(&settings, Type::TEXT), (&secret_ref, Type::TEXT), (&revision, Type::INT8), (&now, Type::INT8)],
        ).await
    } else {
        tx.query_typed(
            "UPDATE pool_providers p SET label=s.label, host=s.host, port=s.port, tls=s.tls, \
             username=s.username, can_read=s.can_read, can_post=s.can_post, \
             max_connections=s.max_connections, stream_reserve=s.stream_reserve, \
             ingest_limit=s.ingest_limit, backbone=s.backbone, tier=s.tier, enabled=s.enabled, \
             secret_ref=$2, revision=$3, updated_at=$4 \
             FROM jsonb_populate_record(NULL::pool_providers, $1::jsonb) s \
             WHERE p.id=s.id AND p.revision=$5 RETURNING p.id::text",
            &[(&settings, Type::TEXT), (&secret_ref, Type::TEXT), (&revision, Type::INT8), (&now, Type::INT8),
              (&(write.expected_revision as i64), Type::INT8)],
        ).await
    }.map_err(|_| StoreError)?;
    if changed.len() != 1 {
        // The transaction (including a fresh secret insert) is rolled back.
        return Ok(PoolProviderWriteOutcome::Conflict);
    }
    let next = PoolProviderRow {
        settings: write.settings.clone(),
        secret_ref,
        revision: revision as u64,
    };
    for (action, details) in
        pool_providers::audit_changes(previous.as_ref(), &next, write.secret.is_some())
    {
        tx.query_typed(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details) \
             VALUES ($1, $2, TRUE, $3, $4, NULL, $5::jsonb)",
            &[
                (&actor_user_id, Type::TEXT),
                (&action, Type::TEXT),
                (
                    &crate::vault::sanitize_source_ip(Some(source_ip)),
                    Type::TEXT,
                ),
                (&now, Type::INT8),
                (&details, Type::TEXT),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    }
    // Delete retired ciphertext only after the pointer has moved, preserving
    // any deliberate references. No plaintext or key material enters audit.
    if let Some(old) = &previous {
        if old.secret_ref != next.secret_ref {
            tx.query_typed(
                "DELETE FROM pool_provider_secrets s WHERE secret_ref=$1 \
                 AND NOT EXISTS (SELECT 1 FROM pool_providers p WHERE p.secret_ref=s.secret_ref)",
                &[(&old.secret_ref, Type::TEXT)],
            )
            .await
            .map_err(|_| StoreError)?;
        }
    }
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(PoolProviderWriteOutcome::Applied(next))
}
