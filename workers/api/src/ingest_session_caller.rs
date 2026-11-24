//! Off-route caller for a single encrypted multipart attempt and completed replay.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::media_envelope::EnvelopeContext;
use nntune_ingest::{
    digest_candidate_store::{
        outcome::{read_completion, stage_completion, OutcomeError},
        CandidateIntent,
    },
    multipart::{
        readback_inventory::{ReadbackScope, VerifiedCompletedReadback},
        session_outcome::{CommittedOutcome, CompletionScope},
        session_upload::{begin, DurableUpload, HandoffError},
    },
};
use tokio_postgres::{Client, Transaction};

use crate::{
    handlers::StoreError,
    ingest_session_auth::{authorize, Action, Grant, Job},
    ingest_session_request::Request,
    native_rest::{NativeRestRecord, Pepper},
    random::secure_random_bytes,
    storage_digest_read::uuid_text,
    worker_entry::{now_millis, with_deadline},
};

const LEASE: &str = include_str!("ingest_session_acquire_write_lease.sql");
const ADVANCE: &str = include_str!("ingest_session_advance_checkpoint.sql");
const AUTHORITY_TIMEOUT_MS: u64 = 3_000;

mod flac;
pub use flac::CommittedFlacUpload;

#[derive(Debug, PartialEq, Eq)]
pub enum UploadError {
    Refused,
    /// The attempt may have made durable progress. Resolve through the scoped
    /// outcome or reconcile the old attempt before starting a fresh lifecycle.
    Unconfirmed,
}

use UploadError::{Refused, Unconfirmed};

/// Load the record, grant and namespace from trusted state for each call.
/// Use the dedicated upload writer; the default API connection is unsuitable.
#[derive(Clone, Copy)]
pub struct Authorization<'a> {
    pub record: &'a NativeRestRecord,
    pub bearer: &'a str,
    pub peppers: &'a [Pepper<'a>],
    pub grant: &'a Grant,
}

/// Current state loaded by the trusted reader from the authorization stores.
pub struct CurrentAuthorization {
    pub record: NativeRestRecord,
    pub grant: Grant,
}

/// Trusted sealer and candidate identity for an existing reservation. The job
/// ID and grant namespace use compact lowercase UUID text in this caller.
#[derive(Clone, Copy)]
pub struct PreparedUpload<'a> {
    pub scope: CompletionScope<'a>,
    pub job: &'a Job<'a>,
    pub request: &'a Request<'a>,
    pub context: &'a EnvelopeContext,
    pub intent: &'a CandidateIntent,
}

impl<'a> PreparedUpload<'a> {
    fn authorize(self, auth: Authorization<'_>) -> Result<i64, UploadError> {
        let scope = self.scope;
        let [_, environment, library, _, seal, _] = self.context.identities;
        let fingerprint = self.request.fingerprint().map_err(|_| Refused)?;
        if (scope.revision, scope.offset) != (0, 0)
            || scope.principal != auth.record.user_id
            || (scope.environment, scope.library, scope.seal) != (environment, library, seal)
            || scope.fingerprint != fingerprint.bytes()
            || self.context.identities[1..] != self.intent.identities
            || self.request.file_bytes != self.context.plaintext_len
            || self.context.plaintext_len != self.intent.plaintext_len
            || self.context.header != self.intent.header
        {
            return Err(Refused);
        }
        authorize(
            (Some(auth.record), Some(auth.bearer)),
            auth.peppers,
            now_millis() / 1000,
            Some(auth.grant),
            (&uuid_text(environment), &uuid_text(library)),
            Action::Upload,
            Some((scope.session, self.job)),
        )
        .map_err(|_| Refused)?;
        i64::try_from(auth.record.expires_at).map_err(|_| Refused)
    }

    fn completed_scope(self) -> CompletionScope<'a> {
        CompletionScope {
            offset: self.request.file_bytes as i64,
            revision: 1,
            ..self.scope
        }
    }
}

pub struct OpenedUpload<'a> {
    prepared: PreparedUpload<'a>,
    lease: String,
    target: DurableUpload<'a>,
}

impl<'a> OpenedUpload<'a> {
    pub fn durable_target(&self) -> &DurableUpload<'a> {
        &self.target
    }

    /// Commit verified readback and the exact final checkpoint together. Pass
    /// freshly loaded authorization and the retention limit from trusted policy.
    /// The post-commit result uses the same trusted reader as replay.
    pub async fn commit(
        self,
        writer: &mut Client,
        auth: Authorization<'_>,
        verified: &VerifiedCompletedReadback<'_, '_>,
        retain_until: i64,
        reader: impl AsyncFnOnce(
            &str,
            &CompletionScope<'_>,
        ) -> Result<Option<CurrentAuthorization>, StoreError>,
    ) -> Result<CommittedOutcome, UploadError> {
        let auth_until = self.prepared.authorize(auth)?;
        let target = self.target.target();
        let inventory = verified.inventory();
        if inventory.bucket() != target.bucket
            || inventory.media().object_key != target.key
            || verified.origin().plaintext_sha256() != &self.prepared.request.content_sha256
        {
            return Err(Refused);
        }
        let scope = self.prepared.completed_scope();
        let mut tx = begin(writer).await.map_err(|_| Unconfirmed)?;
        tx.query_opt(
            ADVANCE,
            &[
                &uuid_text(scope.environment),
                &uuid_text(scope.library),
                &scope.principal,
                &scope.session,
                &scope.fingerprint.as_slice(),
                &uuid_text(scope.seal),
                &0_i64,
                &0_i64,
                &scope.offset,
                &self.lease,
            ],
        )
        .await
        .map_err(|_| Unconfirmed)?
        .ok_or(Refused)?;
        let lease = fresh_id()?;
        acquire(&tx, &scope, &lease).await?;
        stage_completion(
            &mut tx,
            &scope,
            &lease,
            self.prepared.intent,
            ReadbackScope {
                bucket: target.bucket,
                configuration: self.prepared.intent.identities[4],
            },
            verified,
            retain_until,
        )
        .await
        .map_err(outcome_error)?;
        tx.query_opt(
            "SELECT 1 WHERE clock_timestamp() < to_timestamp($1::bigint::double precision)",
            &[&auth_until],
        )
        .await
        .map_err(|_| Unconfirmed)?
        .ok_or(Refused)?;
        tx.commit().await.map_err(|_| Unconfirmed)?;
        read(writer, auth, self.prepared, reader)
            .await
            .map_err(|_| Unconfirmed)?
            .ok_or(Unconfirmed)
    }
}

/// Open once with the signed adapter. Use the returned target and its deadline
/// with the existing signed part, completion, HEAD, sidecar and readback adapters.
/// An in-flight or interrupted attempt must be reconciled before replacement.
#[cfg(target_arch = "wasm32")]
pub async fn open_signed<'a>(
    writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'a>,
    sender: &'a nntune_ingest::multipart::s3_completion::S3CompletionSender,
    key: &'a str,
    until: SystemTime,
) -> Result<OpenedUpload<'a>, UploadError> {
    open(
        writer,
        auth,
        prepared,
        until,
        async |db, scope, lease, deadline| {
            sender
                .open_upload(db, scope, lease, prepared.context, key, deadline)
                .await
        },
    )
    .await
}

/// The adapter must return the target from the scoped, committed handoff.
pub async fn open<'a>(
    writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'a>,
    until: SystemTime,
    start: impl AsyncFnOnce(
        &mut Client,
        &CompletionScope<'_>,
        &str,
        SystemTime,
    ) -> Result<DurableUpload<'a>, HandoffError>,
) -> Result<OpenedUpload<'a>, UploadError> {
    let expiry = prepared.authorize(auth)? as u64;
    let until = until.min(
        UNIX_EPOCH
            .checked_add(Duration::from_secs(expiry))
            .ok_or(Refused)?,
    );
    if UNIX_EPOCH + Duration::from_millis(now_millis()) >= until {
        return Err(Refused);
    }
    let lease = fresh_id()?;
    let tx = begin(writer).await.map_err(|_| Unconfirmed)?;
    acquire(&tx, &prepared.scope, &lease).await?;
    tx.commit().await.map_err(|_| Unconfirmed)?;
    let target = start(writer, &prepared.scope, &lease, until)
        .await
        .map_err(|_| Unconfirmed)?;
    Ok(OpenedUpload {
        prepared,
        lease,
        target,
    })
}

/// Re-read the exact committed pins. After acquiring the scoped session lock,
/// invoke the trusted reader for the credential ID and locked upload scope.
/// It must load current credential and upload grant state from their authorities
/// through separate access, without a cached snapshot or the upload writer's role.
pub async fn read(
    writer: &mut Client,
    auth: Authorization<'_>,
    prepared: PreparedUpload<'_>,
    reader: impl AsyncFnOnce(
        &str,
        &CompletionScope<'_>,
    ) -> Result<Option<CurrentAuthorization>, StoreError>,
) -> Result<Option<CommittedOutcome>, UploadError> {
    prepared.authorize(auth)?;
    read_completion(writer, &prepared.completed_scope(), async |_, locked| {
        let current = with_deadline(
            reader(&auth.record.id, locked),
            AUTHORITY_TIMEOUT_MS,
            StoreError,
        )
        .await
        .map_err(|_| OutcomeError::Indeterminate)?
        .ok_or(OutcomeError::Refused)?;
        if current.record.id != auth.record.id {
            return Err(OutcomeError::Refused);
        }
        prepared
            .authorize(Authorization {
                record: &current.record,
                grant: &current.grant,
                ..auth
            })
            .map_err(|_| OutcomeError::Refused)
    })
    .await
    .map_err(outcome_error)
}

async fn acquire(
    tx: &Transaction<'_>,
    scope: &CompletionScope<'_>,
    lease: &str,
) -> Result<(), UploadError> {
    tx.query_opt(
        LEASE,
        &[
            &uuid_text(scope.environment),
            &uuid_text(scope.library),
            &scope.principal,
            &scope.session,
            &scope.fingerprint.as_slice(),
            &uuid_text(scope.seal),
            &scope.revision,
            &scope.offset,
            &lease,
        ],
    )
    .await
    .map_err(|_| Unconfirmed)?
    .map(|_| ())
    .ok_or(Refused)
}

fn fresh_id() -> Result<String, UploadError> {
    let mut bytes = secure_random_bytes::<16>().map_err(|_| Unconfirmed)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(uuid_text(bytes))
}

fn outcome_error(error: OutcomeError) -> UploadError {
    match error {
        OutcomeError::Refused => Refused,
        OutcomeError::Publication(_) | OutcomeError::Indeterminate => Unconfirmed,
    }
}

#[cfg(all(test, feature = "postgres-tests", not(target_arch = "wasm32")))]
#[path = "../tests/fixtures/ingest_session_caller_postgres.rs"]
mod postgres_tests;
