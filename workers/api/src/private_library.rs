//! Explicit release admission for a selected private-listening invite.
//! The protected caller supplies the authorized issuer and publisher result.

use serde::Deserialize;
use tokio_postgres::{types::Type, Transaction};

use crate::{
    catalogue::{LibraryEntry, CATALOGUE_MAX_PAGE_SIZE},
    handlers::StoreError,
    meter_sink::is_uuid_shaped,
    postgres::PgQuery,
    private_listening::PrivateListening,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    environment_id: String,
    library_id: String,
}

fn canonical_uuid(value: &str) -> bool {
    is_uuid_shaped(value) && !value.bytes().any(|b| b.is_ascii_uppercase())
}

impl Scope {
    pub(crate) fn valid(&self) -> bool {
        canonical_uuid(&self.environment_id) && canonical_uuid(&self.library_id)
    }

    #[cfg(feature = "encrypted-upload-caller")]
    pub(crate) fn matches(&self, environment: [u8; 16], library: [u8; 16]) -> bool {
        use crate::storage_digest_read::uuid_text;
        self.environment_id.replace('-', "") == uuid_text(environment)
            && self.library_id.replace('-', "") == uuid_text(library)
    }
}

#[derive(Clone, Copy)]
pub struct Admission<'a> {
    pub issuer: &'a str,
    pub recipient: &'a str,
    pub invite_id: i64,
    pub track_id: &'a str,
    pub job_id: &'a str,
    pub plaintext_sha256: &'a [u8; 32],
}

/// Call from the protected issuer flow with the exact publisher result.
/// The caller owns step-up, transaction deadlines and commit. A successful
/// result is usable only after commit. A refusal leaves admission unchanged.
pub async fn stage_admission(
    tx: &Transaction<'_>,
    policy: Option<&PrivateListening>,
    admission: Admission<'_>,
) -> Result<bool, StoreError> {
    let Some((policy, scope)) =
        policy.and_then(|policy| policy.library_scope.as_ref().map(|scope| (policy, scope)))
    else {
        return Ok(false);
    };
    if policy.owner_user_id != admission.issuer
        || !policy.invite_ids.contains(&admission.invite_id)
        || !canonical_uuid(admission.track_id)
        || !crate::ledger::is_valid_key_field(admission.recipient, 128)
        || !crate::ledger::is_valid_key_field(admission.job_id, 64)
    {
        return Ok(false);
    }
    tx.query_typed_one(
        include_str!("private_library_admit.sql"),
        &[
            (&admission.recipient, Type::TEXT),
            (&admission.issuer, Type::TEXT),
            (&admission.invite_id, Type::INT8),
            (&admission.track_id, Type::TEXT),
            (&admission.job_id, Type::TEXT),
            (&scope.environment_id, Type::TEXT),
            (&scope.library_id, Type::TEXT),
            (&admission.plaintext_sha256.as_slice(), Type::BYTEA),
            (
                &i64::try_from(policy.expires_at).map_err(|_| StoreError)?,
                Type::INT8,
            ),
        ],
    )
    .await
    .map_err(|_| StoreError)?
    .try_get(0)
    .map_err(|_| StoreError)
}

pub(crate) async fn page(
    db: &impl PgQuery,
    user: &str,
    policy: &PrivateListening,
) -> Result<Vec<LibraryEntry>, StoreError> {
    let Some(scope) = &policy.library_scope else {
        return Ok(Vec::new());
    };
    let limit = i64::try_from(CATALOGUE_MAX_PAGE_SIZE).map_err(|_| StoreError)?;
    db.query_typed(
        concat!(
            "WITH access(allowed) AS (",
            include_str!("private_listening_access.sql"),
            ") ",
            include_str!("private_library_page.sql")
        ),
        &[
            (&user, Type::TEXT),
            (&policy.owner_user_id, Type::TEXT),
            (&policy.invite_ids, Type::INT8_ARRAY),
            (&scope.environment_id, Type::TEXT),
            (&scope.library_id, Type::TEXT),
            (
                &i64::try_from(policy.expires_at).map_err(|_| StoreError)?,
                Type::INT8,
            ),
            (&limit, Type::INT8),
        ],
    )
    .await
    .map_err(|_| StoreError)?
    .iter()
    .map(|row| {
        let id: String = row.try_get(0).map_err(|_| StoreError)?;
        let position = |column| -> Result<Option<i16>, StoreError> {
            Ok(i16::try_from(row.try_get::<_, i32>(column).map_err(|_| StoreError)?).ok())
        };
        Ok(LibraryEntry {
            occurrence_key: format!("private:{id}"),
            id,
            like_target_id: None,
            recording_mbids: row
                .try_get::<_, Option<String>>(1)
                .map_err(|_| StoreError)?
                .into_iter()
                .collect(),
            release_mbid: row.try_get(2).map_err(|_| StoreError)?,
            title: row.try_get(3).map_err(|_| StoreError)?,
            duration_ms: row.try_get(4).map_err(|_| StoreError)?,
            track_no: position(5usize)?,
            medium_no: position(6usize)?,
            artist: None,
            artist_mbid: None,
            release: None,
            art_url: None,
        })
    })
    .collect()
}
