//! Hyperdrive persistence for the production credential core. Not an HTTP mount.
use super::{PgStore, StoreError, TOTAL_TIMEOUT_MS};
use crate::field_envelope::FieldEnvelope;
use crate::subsonic_auth::{
    AuthError, AuthMode, CredentialRecord, CredentialStore, Reindex, TokenEnvelope,
};
use tokio_postgres::{types::Type, Client, GenericClient, Row, Transaction};

const COLUMNS: &str = "c.id::text AS id, c.user_id, c.subsonic_username, c.scope, c.auth_mode, \
    c.api_key_hmac, c.pepper_version, c.token_secret_encrypted, c.token_envelope_version, \
    c.token_payload_format, c.token_kek_version, c.token_dek_wrapped_nonce, \
    c.token_dek_wrapped, c.token_payload_nonce";
// Compare all security-relevant selected material, not just id or last_used_at.
const SNAPSHOT: &str = "id = $1::text::uuid AND user_id = $2 AND subsonic_username = $3 \
    AND scope = 'subsonic' AND revoked_at IS NULL AND auth_mode = $4 \
    AND api_key_hmac IS NOT DISTINCT FROM $5 AND pepper_version IS NOT DISTINCT FROM $6 \
    AND token_secret_encrypted IS NOT DISTINCT FROM $7 \
    AND token_envelope_version IS NOT DISTINCT FROM $8 \
    AND token_payload_format IS NOT DISTINCT FROM $9 \
    AND token_kek_version IS NOT DISTINCT FROM $10 \
    AND token_dek_wrapped_nonce IS NOT DISTINCT FROM $11 \
    AND token_dek_wrapped IS NOT DISTINCT FROM $12 \
    AND token_payload_nonce IS NOT DISTINCT FROM $13";

fn record(row: &Row) -> Result<CredentialRecord, StoreError> {
    let mode: String = row.try_get("auth_mode").map_err(|_| StoreError)?;
    let auth_mode = match mode.as_str() {
        "apikey" => AuthMode::ApiKey,
        "token" => AuthMode::Token,
        "both" => AuthMode::Both,
        _ => return Err(StoreError),
    };
    let ciphertext: Option<Vec<u8>> = row
        .try_get("token_secret_encrypted")
        .map_err(|_| StoreError)?;
    let token = if let Some(ciphertext) = ciphertext {
        let wrap_nonce: Vec<u8> = row
            .try_get("token_dek_wrapped_nonce")
            .map_err(|_| StoreError)?;
        let nonce: Vec<u8> = row.try_get("token_payload_nonce").map_err(|_| StoreError)?;
        Some(TokenEnvelope {
            envelope_version: u16::try_from(
                row.try_get::<_, i16>("token_envelope_version")
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?,
            payload_format: u16::try_from(
                row.try_get::<_, i16>("token_payload_format")
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?,
            kek_version: u32::try_from(
                row.try_get::<_, i32>("token_kek_version")
                    .map_err(|_| StoreError)?,
            )
            .map_err(|_| StoreError)?,
            envelope: FieldEnvelope {
                dek_wrapped_nonce: wrap_nonce.try_into().map_err(|_| StoreError)?,
                dek_wrapped: row.try_get("token_dek_wrapped").map_err(|_| StoreError)?,
                payload_nonce: nonce.try_into().map_err(|_| StoreError)?,
                ciphertext,
            },
        })
    } else {
        None
    };
    Ok(CredentialRecord {
        id: row.try_get("id").map_err(|_| StoreError)?,
        user_id: row.try_get("user_id").map_err(|_| StoreError)?,
        username: row.try_get("subsonic_username").map_err(|_| StoreError)?,
        scope: row.try_get("scope").map_err(|_| StoreError)?,
        auth_mode,
        revoked: false,
        owner_active: true,
        api_key_hmac: row.try_get("api_key_hmac").map_err(|_| StoreError)?,
        pepper_version: row
            .try_get::<_, Option<i32>>("pepper_version")
            .map_err(|_| StoreError)?
            .map(u32::try_from)
            .transpose()
            .map_err(|_| StoreError)?,
        token,
    })
}

async fn transaction(client: &mut Client) -> Result<Transaction<'_>, StoreError> {
    let tx = client.transaction().await.map_err(|_| StoreError)?;
    // Hyperdrive transaction pooling requires the timeout on this transaction.
    tx.batch_execute(&format!("SET LOCAL statement_timeout = {TOTAL_TIMEOUT_MS}"))
        .await
        .map_err(|_| StoreError)?;
    Ok(tx)
}

async fn lookup(
    client: &mut Client,
    version: Option<u32>,
    value: &str,
) -> Result<Option<CredentialRecord>, StoreError> {
    let tx = transaction(client).await?;
    let sql = format!(
        "SELECT {COLUMNS} FROM subsonic_credentials c JOIN users u ON u.id = c.user_id \
        WHERE c.scope = 'subsonic' AND c.revoked_at IS NULL AND u.deleting_at IS NULL AND {}",
        if version.is_some() {
            "c.pepper_version = $1 AND c.api_key_hmac = $2"
        } else {
            "c.subsonic_username = $1"
        }
    );
    let rows = if let Some(version) = version {
        tx.query_typed(
            &sql,
            &[
                (&i32::try_from(version).map_err(|_| StoreError)?, Type::INT4),
                (&value, Type::TEXT),
            ],
        )
        .await
    } else {
        tx.query_typed(&sql, &[(&value, Type::TEXT)]).await
    }
    .map_err(|_| StoreError)?;
    if rows.len() > 1 {
        return Err(StoreError);
    }
    let selected = rows.first().map(record).transpose()?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(selected)
}

async fn owner_fence(
    tx: &impl GenericClient,
    selected: &CredentialRecord,
) -> Result<bool, StoreError> {
    if selected.scope != "subsonic" || selected.revoked || !selected.owner_active {
        return Ok(false);
    }
    // Account erasure locks users first. FOR SHARE fences deleting_at updates
    // through commit; KEY SHARE would not conflict with that non-key update.
    let rows = tx
        .query_typed(
            "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR SHARE",
            &[(&selected.user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    Ok(rows.len() == 1)
}

async fn mutate(
    client: &mut Client,
    s: &CredentialRecord,
    completion: Option<Option<&Reindex>>,
    now: u64,
) -> Result<bool, StoreError> {
    let now = i64::try_from(now).map_err(|_| StoreError)?;
    let tx = transaction(client).await?;
    if !owner_fence(&tx, s).await? {
        return Ok(false);
    }
    let mode = match s.auth_mode {
        AuthMode::ApiKey => "apikey",
        AuthMode::Token => "token",
        AuthMode::Both => "both",
    };
    let pepper = s
        .pepper_version
        .map(i32::try_from)
        .transpose()
        .map_err(|_| StoreError)?;
    let ciphertext = s.token.as_ref().map(|t| t.envelope.ciphertext.as_slice());
    let envelope_version = s
        .token
        .as_ref()
        .map(|t| i16::try_from(t.envelope_version))
        .transpose()
        .map_err(|_| StoreError)?;
    let payload_format = s
        .token
        .as_ref()
        .map(|t| i16::try_from(t.payload_format))
        .transpose()
        .map_err(|_| StoreError)?;
    let kek = s
        .token
        .as_ref()
        .map(|t| i32::try_from(t.kek_version))
        .transpose()
        .map_err(|_| StoreError)?;
    let wrap_nonce = s
        .token
        .as_ref()
        .map(|t| t.envelope.dek_wrapped_nonce.as_slice());
    let wrapped = s.token.as_ref().map(|t| t.envelope.dek_wrapped.as_slice());
    let nonce = s
        .token
        .as_ref()
        .map(|t| t.envelope.payload_nonce.as_slice());
    let mut params: Vec<(&(dyn tokio_postgres::types::ToSql + Sync), Type)> = vec![
        (&s.id, Type::TEXT),
        (&s.user_id, Type::TEXT),
        (&s.username, Type::TEXT),
        (&mode, Type::TEXT),
        (&s.api_key_hmac, Type::TEXT),
        (&pepper, Type::INT4),
        (&ciphertext, Type::BYTEA),
        (&envelope_version, Type::INT2),
        (&payload_format, Type::INT2),
        (&kek, Type::INT4),
        (&wrap_nonce, Type::BYTEA),
        (&wrapped, Type::BYTEA),
        (&nonce, Type::BYTEA),
        (&now, Type::INT8),
    ];
    let new_version = completion
        .flatten()
        .map(|r| i32::try_from(r.pepper_version))
        .transpose()
        .map_err(|_| StoreError)?;
    let new_digest = completion.flatten().map(|r| r.api_key_hmac.as_str());
    let sql = if completion.is_some() {
        params.extend([
            (
                &new_version as &(dyn tokio_postgres::types::ToSql + Sync),
                Type::INT4,
            ),
            (&new_digest, Type::TEXT),
        ]);
        format!("UPDATE subsonic_credentials SET last_used_at = GREATEST(last_used_at, to_timestamp($14::bigint::double precision)), \
            pepper_version = COALESCE($15, pepper_version), api_key_hmac = COALESCE($16, api_key_hmac) \
            WHERE {SNAPSHOT} RETURNING id")
    } else {
        // Fixed per-credential 60/minute allowance, persisted across isolates.
        // A regressed clock never reopens an earlier allowance.
        format!(
            "UPDATE subsonic_credentials SET \
            attempt_count = CASE WHEN attempt_window < $14 / 60 THEN 1 ELSE attempt_count + 1 END, \
            attempt_window = GREATEST(attempt_window, $14 / 60) \
            WHERE {SNAPSHOT} AND (attempt_window < $14 / 60 OR attempt_count < 60) RETURNING id"
        )
    };
    let rows = tx
        .query_typed(&sql, &params)
        .await
        .map_err(|_| StoreError)?;
    if rows.len() != 1 {
        return Ok(false);
    }
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(true)
}

impl CredentialStore for PgStore {
    async fn by_key(
        &self,
        version: u32,
        digest: &str,
    ) -> Result<Option<CredentialRecord>, AuthError> {
        Self::bounded(
            async { lookup(&mut self.connect().await?, Some(version), digest).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
        .map_err(|_| AuthError::Unavailable)
    }
    async fn by_username(&self, username: &str) -> Result<Option<CredentialRecord>, AuthError> {
        Self::bounded(
            async { lookup(&mut self.connect().await?, None, username).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
        .map_err(|_| AuthError::Unavailable)
    }
    async fn admit_attempt(
        &self,
        selected: &CredentialRecord,
        now: u64,
    ) -> Result<bool, AuthError> {
        Self::bounded(
            async { mutate(&mut self.connect().await?, selected, None, now).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
        .map_err(|_| AuthError::Unavailable)
    }
    async fn complete(
        &self,
        selected: &CredentialRecord,
        reindex: Option<&Reindex>,
        now: u64,
    ) -> Result<bool, AuthError> {
        Self::bounded(
            async { mutate(&mut self.connect().await?, selected, Some(reindex), now).await },
            TOTAL_TIMEOUT_MS,
        )
        .await
        .map_err(|_| AuthError::Unavailable)
    }
}

const MAX_ACTIVE_CREDENTIALS: i64 = 32;

pub(super) async fn insert(
    client: &mut Client,
    prepared: &crate::subsonic_lifecycle::PreparedCredential,
    session: &crate::session::SessionClaims,
) -> Result<bool, StoreError> {
    let s = &prepared.record;
    let tx = transaction(client).await?;
    // Serialize issue/cap checks per owner and fence account deletion through
    // commit. Every credential mutation follows users-first lock order.
    let owners = tx
        .query_typed(
            "SELECT id FROM users WHERE id = $1 AND deleting_at IS NULL FOR UPDATE",
            &[(&s.user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if owners.len() != 1 {
        return Ok(false);
    }
    if session.user_id != s.user_id
        || prepared.created_at >= session.expires_at
        || prepared.created_at < session.created_at
        || prepared.created_at - session.created_at > crate::session::PRIVILEGE_REAUTH_WINDOW_SECS
    {
        return Ok(false);
    }
    let created = i64::try_from(session.created_at).map_err(|_| StoreError)?;
    let expires = i64::try_from(session.expires_at).map_err(|_| StoreError)?;
    let counter = i32::try_from(session.rotation_counter).map_err(|_| StoreError)?;
    let sessions = tx
        .query_typed(
            "SELECT id FROM sessions WHERE id = $1 AND user_id = $2 AND NOT revoked \
         AND created_at = $3 AND expires_at = $4 AND rotation_counter = $5 FOR SHARE",
            &[
                (&session.session_id, Type::TEXT),
                (&s.user_id, Type::TEXT),
                (&created, Type::INT8),
                (&expires, Type::INT8),
                (&counter, Type::INT4),
            ],
        )
        .await
        .map_err(|_| StoreError)?;
    if sessions.len() != 1 {
        return Ok(false);
    }
    let count: i64 = tx.query_typed(
        "SELECT count(*)::bigint FROM subsonic_credentials WHERE user_id = $1 AND revoked_at IS NULL",
        &[(&s.user_id, Type::TEXT)],
    ).await.map_err(|_| StoreError)?[0].try_get(0).map_err(|_| StoreError)?;
    if count >= MAX_ACTIVE_CREDENTIALS {
        return Ok(false);
    }
    let mode = match s.auth_mode {
        AuthMode::ApiKey => "apikey",
        AuthMode::Token => "token",
        AuthMode::Both => "both",
    };
    let pepper = s
        .pepper_version
        .map(i32::try_from)
        .transpose()
        .map_err(|_| StoreError)?;
    let kek = s
        .token
        .as_ref()
        .map(|t| i32::try_from(t.kek_version))
        .transpose()
        .map_err(|_| StoreError)?;
    let envelope_version = s
        .token
        .as_ref()
        .map(|t| i16::try_from(t.envelope_version))
        .transpose()
        .map_err(|_| StoreError)?;
    let payload_format = s
        .token
        .as_ref()
        .map(|t| i16::try_from(t.payload_format))
        .transpose()
        .map_err(|_| StoreError)?;
    let ciphertext = s.token.as_ref().map(|t| t.envelope.ciphertext.as_slice());
    let wrap_nonce = s
        .token
        .as_ref()
        .map(|t| t.envelope.dek_wrapped_nonce.as_slice());
    let wrapped = s.token.as_ref().map(|t| t.envelope.dek_wrapped.as_slice());
    let nonce = s
        .token
        .as_ref()
        .map(|t| t.envelope.payload_nonce.as_slice());
    let now = i64::try_from(prepared.created_at).map_err(|_| StoreError)?;
    let rows = tx.query_typed(
        "INSERT INTO subsonic_credentials (id,user_id,label,subsonic_username,scope,auth_mode,api_key_hmac,pepper_version,\
         token_secret_encrypted,token_envelope_version,token_payload_format,token_kek_version,token_dek_wrapped_nonce,\
         token_dek_wrapped,token_payload_nonce,created_at) VALUES ($1::text::uuid,$2,$3,$4,'subsonic',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,\
         to_timestamp($15::bigint::double precision)) RETURNING id",
        &[
            (&s.id, Type::TEXT), (&s.user_id, Type::TEXT), (&prepared.label, Type::TEXT), (&s.username, Type::TEXT),
            (&mode, Type::TEXT), (&s.api_key_hmac, Type::TEXT), (&pepper, Type::INT4), (&ciphertext, Type::BYTEA),
            (&envelope_version, Type::INT2), (&payload_format, Type::INT2), (&kek, Type::INT4),
            (&wrap_nonce, Type::BYTEA), (&wrapped, Type::BYTEA), (&nonce, Type::BYTEA), (&now, Type::INT8),
        ],
    ).await.map_err(|_| StoreError)?;
    if rows.len() != 1 {
        return Err(StoreError);
    }
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(true)
}

pub(super) async fn list(
    client: &mut Client,
    user_id: &str,
) -> Result<Vec<crate::subsonic_lifecycle::ListedCredential>, StoreError> {
    let tx = transaction(client).await?;
    // Metadata projection cannot accidentally serialize HMACs or envelopes.
    let rows = tx.query_typed(
        "SELECT c.id::text, c.label, c.auth_mode, EXTRACT(EPOCH FROM c.created_at)::bigint AS created_at, \
         EXTRACT(EPOCH FROM c.last_used_at)::bigint AS last_used_at, EXTRACT(EPOCH FROM c.revoked_at)::bigint AS revoked_at \
         FROM subsonic_credentials c JOIN users u ON u.id = c.user_id \
         WHERE c.user_id = $1 AND c.scope = 'subsonic' AND u.deleting_at IS NULL \
         ORDER BY (c.revoked_at IS NULL) DESC, c.created_at DESC, c.id LIMIT 100",
        &[(&user_id, Type::TEXT)],
    ).await.map_err(|_| StoreError)?;
    let listed = rows
        .iter()
        .map(|r| {
            Ok(crate::subsonic_lifecycle::ListedCredential {
                id: r.try_get("id").map_err(|_| StoreError)?,
                label: r.try_get("label").map_err(|_| StoreError)?,
                auth_mode: r.try_get("auth_mode").map_err(|_| StoreError)?,
                created_at: u64::try_from(
                    r.try_get::<_, i64>("created_at").map_err(|_| StoreError)?,
                )
                .map_err(|_| StoreError)?,
                last_used_at: r
                    .try_get::<_, Option<i64>>("last_used_at")
                    .map_err(|_| StoreError)?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| StoreError)?,
                revoked_at: r
                    .try_get::<_, Option<i64>>("revoked_at")
                    .map_err(|_| StoreError)?
                    .map(u64::try_from)
                    .transpose()
                    .map_err(|_| StoreError)?,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(listed)
}

pub(super) async fn revoke(
    client: &mut Client,
    id: Option<&str>,
    user_id: &str,
    now: u64,
) -> Result<u64, StoreError> {
    let now = i64::try_from(now).map_err(|_| StoreError)?;
    let tx = transaction(client).await?;
    let owners = tx
        .query_typed(
            "SELECT id FROM users WHERE id = $1 FOR UPDATE",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    if owners.len() != 1 {
        return Ok(0);
    }
    if id.is_none() {
        // Recovery closes the issuance session and compatibility keys in one
        // owner-serialized transaction; a pre-read session cannot mint after it.
        tx.query_typed(
            "UPDATE sessions SET revoked = TRUE WHERE user_id = $1",
            &[(&user_id, Type::TEXT)],
        )
        .await
        .map_err(|_| StoreError)?;
    }
    let rows = tx.query_typed(
        "UPDATE subsonic_credentials SET revoked_at = COALESCE(revoked_at, to_timestamp($3::bigint::double precision)) \
         WHERE user_id = $1 AND scope = 'subsonic' AND ($2::text IS NULL OR id = $2::text::uuid) RETURNING id",
        &[(&user_id, Type::TEXT), (&id, Type::TEXT), (&now, Type::INT8)],
    ).await.map_err(|_| StoreError)?;
    tx.commit().await.map_err(|_| StoreError)?;
    Ok(rows.len() as u64)
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "subsonic_pg_tests.rs"]
mod postgres_tests;
