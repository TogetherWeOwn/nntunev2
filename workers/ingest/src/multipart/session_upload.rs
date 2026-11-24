//! Dormant handoff from a scoped write lease to a committed S3 upload ID.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::{media_envelope::EnvelopeContext, s3_locator::S3MediaMetadata};
use tokio_postgres::{error::SqlState, Client, IsolationLevel, Transaction};

use super::{
    initiation::InitiatedUpload, resume_checkpoint::ResumeBinding,
    session_completion::valid_upload_id, session_outcome::CompletionScope, UploadTarget,
};
use crate::digest_candidate_store::uuid;
use HandoffError::{Indeterminate, Pending, Refused};

const LOCK: &str = include_str!("session_upload_lock.sql");
const CLAIM: &str = "UPDATE public.ingest_session_reservations
    SET multipart_object_id = $2::text::uuid, multipart_bucket = $3
    WHERE id = $1::text::uuid AND multipart_object_id IS NULL
      AND clock_timestamp() < LEAST(idle_deadline, absolute_deadline)";
const RECORD: &str = "UPDATE public.ingest_session_reservations
    SET multipart_upload_id = $2
    WHERE id = $1::text::uuid AND multipart_upload_id IS NULL
      AND clock_timestamp() < LEAST(idle_deadline, absolute_deadline)";

#[derive(Debug, PartialEq, Eq)]
pub enum HandoffError {
    Refused,
    /// Initiation may be in flight or its ID may have been lost. Re-read or
    /// reconcile under the session fence. Never send another initiation.
    Pending,
    /// The store or commit was unconfirmed. Retry only via the scoped handoff.
    Indeterminate,
}

/// A committed target for the supplied checkpoint and write lease. Recover
/// again with the current lease on resume and respect its deadline on sends.
pub struct DurableUpload<'a> {
    bucket: &'a str,
    key: &'a str,
    id: String,
    usable_until: SystemTime,
    context: &'a EnvelopeContext,
    session: String,
}

impl<'a> DurableUpload<'a> {
    #[cfg(all(target_arch = "wasm32", feature = "s3-completion-wasm-test"))]
    pub(super) fn new(
        target: UploadTarget<'a>,
        context: &'a EnvelopeContext,
        session: &'a str,
        usable_until: SystemTime,
    ) -> Self {
        Self {
            bucket: target.bucket,
            key: target.key,
            id: target.upload_id.to_owned(),
            context,
            session: session.to_owned(),
            usable_until,
        }
    }

    pub fn target(&self) -> UploadTarget<'_> {
        UploadTarget {
            bucket: self.bucket,
            key: self.key,
            upload_id: &self.id,
        }
    }

    pub fn usable_until(&self) -> SystemTime {
        self.usable_until
    }

    pub(super) fn matches(&self, binding: ResumeBinding<'_>, context: &EnvelopeContext) -> bool {
        self.target() == binding.plan.target
            && self.session == binding.session
            && self.context.identities[1..] == context.identities[1..]
            && self.context.header == context.header
            && self.context.plaintext_len == context.plaintext_len
    }
}

/// Commit the scoped object claim and provider ID before returning a target.
/// The initiator must use the exact trusted configuration, bucket and key.
pub async fn handoff<'a>(
    db: &mut Client,
    scope: &CompletionScope<'_>,
    lease: &str,
    context: &'a EnvelopeContext,
    target: (&'a str, &'a str),
    usable_until: SystemTime,
    initiate: impl AsyncFnOnce(SystemTime) -> Result<InitiatedUpload<'a>, ()>,
) -> Result<DurableUpload<'a>, HandoffError> {
    let (bucket, key) = target;
    let [_, environment, library, object, generation, configuration] = context.identities;
    if (environment, library, generation) != (scope.environment, scope.library, scope.seal)
        || !S3MediaMetadata::matches_target(bucket, key, context)
    {
        return Err(Refused);
    }
    let object = uuid(&object);
    let configuration_id = uuid(&configuration);
    let length = i64::try_from(context.plaintext_len).map_err(|_| Refused)?;
    let lock = async |tx: &Transaction<'_>| {
        let row = tx
            .query_opt(
                LOCK,
                &[
                    &uuid(&environment),
                    &uuid(&library),
                    &scope.principal,
                    &scope.session,
                    &scope.fingerprint.as_slice(),
                    &uuid(&generation),
                    &scope.revision,
                    &scope.offset,
                    &lease,
                    &object,
                    &configuration_id,
                    &bucket,
                    &length,
                    &context.header.as_slice(),
                ],
            )
            .await
            .map_err(db_error)?
            .ok_or(Refused)?;
        let millis =
            u64::try_from(row.try_get::<_, i64>(2).map_err(db_error)?).map_err(|_| Refused)?;
        let deadline = UNIX_EPOCH
            .checked_add(Duration::from_millis(millis))
            .ok_or(Refused)?;
        Ok((
            row.try_get::<_, bool>(0).map_err(db_error)?,
            row.try_get::<_, Option<String>>(1).map_err(db_error)?,
            deadline.min(usable_until),
        ))
    };
    let upload = |id: String, usable_until| {
        if !valid_upload_id(&id) {
            return Err(Indeterminate);
        }
        Ok(DurableUpload {
            bucket,
            key,
            id,
            usable_until,
            context,
            session: scope.session.to_owned(),
        })
    };

    let tx = begin(db).await?;
    let (claimed, id, deadline) = lock(&tx).await?;
    if claimed {
        tx.commit().await.map_err(db_error)?;
        return upload(id.ok_or(Pending)?, deadline);
    }
    if tx
        .execute(CLAIM, &[&scope.session, &object, &bucket])
        .await
        .map_err(db_error)?
        != 1
    {
        return Err(Refused);
    }
    tx.commit().await.map_err(db_error)?;

    // A committed claim is exclusive even if this future is dropped, the
    // response is lost, the lease is consumed, or the second commit fails.
    let receipt = initiate(deadline).await.map_err(|()| Pending)?;
    let target = receipt.target();
    if receipt.configuration() != configuration || target.bucket != bucket || target.key != key {
        return Err(Pending);
    }
    let tx = begin(db).await?;
    let (true, None, deadline) = lock(&tx).await? else {
        return Err(Refused);
    };
    if tx
        .execute(RECORD, &[&scope.session, &target.upload_id])
        .await
        .map_err(db_error)?
        != 1
    {
        return Err(Refused);
    }
    tx.commit().await.map_err(db_error)?;
    upload(target.upload_id.to_owned(), deadline)
}

pub async fn begin(db: &mut Client) -> Result<Transaction<'_>, HandoffError> {
    let tx = db
        .build_transaction()
        .isolation_level(IsolationLevel::ReadCommitted)
        .start()
        .await
        .map_err(db_error)?;
    tx.batch_execute("SET LOCAL transaction_timeout = '10s'; SET LOCAL synchronous_commit = on")
        .await
        .map_err(db_error)?;
    Ok(tx)
}

fn db_error(error: tokio_postgres::Error) -> HandoffError {
    if error.code().is_some_and(|code| {
        *code == SqlState::UNIQUE_VIOLATION || *code == SqlState::INVALID_TEXT_REPRESENTATION
    }) {
        Refused
    } else {
        Indeterminate
    }
}

#[cfg(test)]
#[path = "../../tests/fixtures/session_upload_postgres.rs"]
mod tests;
