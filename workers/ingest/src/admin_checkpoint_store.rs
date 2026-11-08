//! Internal PostgreSQL stage/control/publication authority. No live dispatch or
//! public route uses this adapter yet. Ordered schema integration, upload
//! initialization and authenticated control/reconciliation routes remain required
//! integration. Null-only reconciliation below is an explicit internal primitive.
//! The caller supplies the authenticated original, captures the trusted queue
//! generation, and applies its existing database/service deadline. No KV fallback.

use crate::{
    admin_checkpoint::{Identity, Snapshot, MAX_CHECKPOINT_BYTES},
    admin_execution::{
        controlled_checkpoint, service_failure_checkpoint, validate_publication,
        validate_stage_transition, Control, ServiceFailureOutcome, StageError, StageOutcome,
        StageRuntime,
    },
    admin_writers::{write_album, AlbumWrite, FileGeometry, SourceSealer, WriteError},
    jobs::{self, JobCheckpoint, JobState},
    matching::MbRecording,
};
use tokio_postgres::{Client, GenericClient};

/// Borrow exclusively from the API/keyring's authenticated original holder,
/// never a queue body or request. Like run_stage/AlbumWrite, this internal seam
/// is not an authorization or decryption boundary. No payload clone or Debug.
pub struct StageOriginal<'a> {
    pub job_id: &'a str,
    pub idempotency_key: &'a str,
    pub nzb_bytes: usize,
    pub parsed: &'a nntune_core::nzb::Document,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    Invalid,
    Stale,
    Database,
    ActorNotAdmin,
    AccountDeleting,
}

/// Trusted session actor and server clock, never queue/request authority. The
/// route must authenticate before opening the original and gate archive resume
/// on the existing capability. SQL rechecks role/deletion at mutation time.
/// IP has a typed representation so audit cannot retain arbitrary header text.
#[derive(Clone, Copy)]
pub struct ControlActor<'a> {
    pub user_id: &'a str,
    pub source_ip: Option<std::net::IpAddr>,
    pub now: i64,
}

impl ControlActor<'_> {
    fn validate(&self) -> Result<(), StoreError> {
        if !crate::dispatch::validate_user_id(self.user_id) || self.now < 0 {
            return Err(StoreError::Invalid);
        }
        Ok(())
    }
}

// Match the API's admin writer lock order. Demotion/deletion either commits
// first and is observed here or waits for this whole mutation/audit transaction.
// No provider work or queue send belongs under these locks.
async fn lock_actor(tx: &impl GenericClient, actor: ControlActor<'_>) -> Result<(), StoreError> {
    tx.query(
        "SELECT pg_advisory_xact_lock(hashtext('admin_writes'))",
        &[],
    )
    .await
    .map_err(|_| StoreError::Database)?;
    let rows = tx
        .query(
            "SELECT role, deleting_at IS NOT NULL AS deleting FROM users WHERE id = $1 FOR UPDATE",
            &[&actor.user_id],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if rows.len() > 1 {
        return Err(StoreError::Invalid);
    }
    let Some(row) = rows.first() else {
        return Err(StoreError::ActorNotAdmin);
    };
    let role: Option<String> = row.try_get("role").map_err(|_| StoreError::Invalid)?;
    if role.as_deref() != Some("admin") {
        return Err(StoreError::ActorNotAdmin);
    }
    let deleting: bool = row.try_get("deleting").map_err(|_| StoreError::Invalid)?;
    if deleting {
        return Err(StoreError::AccountDeleting);
    }
    Ok(())
}

async fn audit_control(
    tx: &impl GenericClient,
    actor: ControlActor<'_>,
    operation: &str,
    previous_status: &str,
    previous_revision: i64,
    successor: &Snapshot,
) -> Result<(), StoreError> {
    let source_ip = actor
        .source_ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "unknown".into());
    let count = tx
        .execute(
            "INSERT INTO audit_log (user_id, action, ok, source_ip, at, subject_user_id, details)
         VALUES ($1, 'ingest.controlled', TRUE, $2, $3, NULL,
             jsonb_build_object('job_id', $4::text, 'operation', $5::text,
                 'previous_status', $6::text, 'status', $7::text,
                 'previous_revision', $8::bigint, 'revision', $9::bigint,
                 'generation', $10::bigint))",
            &[
                &actor.user_id,
                &source_ip,
                &actor.now,
                &successor.checkpoint().job_id,
                &operation,
                &previous_status,
                &successor.status(),
                &previous_revision,
                &successor.revision(),
                &successor.generation(),
            ],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if count != 1 {
        return Err(StoreError::Database);
    }
    Ok(())
}

/// A bounded wake hint, not checkpoint or execution authority. No payload,
/// uploader, provider, stage or attempt rides on the queue. Kept separate from
/// legacy QueueMsg so its KV consumer cannot initialize or execute admin work.
/// No Debug: consumers report fixed error categories, never raw message bodies.
#[derive(Clone, PartialEq, Eq)]
pub struct Delivery(DeliveryWire);

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryWire {
    schema_version: u8,
    job_id: String,
    idempotency_key: String,
    generation: i64,
}

impl Delivery {
    pub fn job_id(&self) -> &str {
        &self.0.job_id
    }

    pub fn idempotency_key(&self) -> &str {
        &self.0.idempotency_key
    }

    pub fn generation(&self) -> i64 {
        self.0.generation
    }

    /// Strict wire decoding only. The authenticated original reader and SQL
    /// authority must still bind this hint before any provider work or mutation.
    pub fn decode(encoded: &str) -> Result<Self, StoreError> {
        if encoded.len() > 512 {
            return Err(StoreError::Invalid);
        }
        let wire: DeliveryWire = serde_json::from_str(encoded).map_err(|_| StoreError::Invalid)?;
        if wire.schema_version != 1
            || wire.generation <= 0
            || !jobs::validate_job_id(&wire.job_id)
            || !crate::library::valid_admin_key(&wire.idempotency_key)
        {
            return Err(StoreError::Invalid);
        }
        Ok(Self(wire))
    }

    pub fn encode(&self) -> Result<String, StoreError> {
        serde_json::to_string(&self.0).map_err(|_| StoreError::Invalid)
    }
}

fn executable(snapshot: &Snapshot) -> bool {
    matches!(
        snapshot.checkpoint().state,
        JobState::Queued
            | JobState::Parsing
            | JobState::Matching
            | JobState::Probing
            | JobState::Writing
    )
}

/// Prepare initial send, progress continuation or explicit recovery only from
/// committed SQL authority for the generation the caller already captured. A
/// send failure/unknown send result retries this same generation, never reads
/// and adopts the latest one. Manual commit uncertainty requires resolving that
/// action separately; it does not authorize selecting a newer generation here.
///
/// Sending happens after this read, outside a transaction. Duplicate sends are
/// safe wake hints: the consumer reloads current authority and every subsequent
/// side effect still uses full-snapshot CAS. No delivery/ack marker is written,
/// so a lost continuation can be prepared again without resetting progress.
/// This is not a scanner, queue binding, automatic reconciliation or live route.
pub async fn prepare_delivery(
    client: &Client,
    original: &StageOriginal<'_>,
    generation: i64,
) -> Result<Option<Delivery>, StoreError> {
    let snapshot = load(client, original, generation).await?;
    Ok(executable(&snapshot).then(|| {
        Delivery(DeliveryWire {
            schema_version: 1,
            job_id: original.job_id.into(),
            idempotency_key: original.idempotency_key.into(),
            generation: snapshot.generation(),
        })
    }))
}

/// The future queue adapter sends only the encoded wake hint. An error can mean
/// rejection or accepted-but-response-lost; callers must treat both as uncertain.
/// This seam installs no live Queue binding and grants no authentication.
pub trait DeliverySender {
    type Error;

    fn send(
        &mut self,
        encoded: String,
    ) -> impl std::future::Future<Output = Result<(), Self::Error>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryError {
    Authority(StoreError),
    Send,
}

/// Reload committed authority, then send outside any transaction. True means
/// the sender confirmed acceptance, not that execution or publication occurred.
/// False means the current generation is parked/terminal. On failure, timeout
/// or cancellation, recovery repeats this call with the SAME captured generation.
/// No acknowledgment marker/reset can make progress disappear after a lost send.
pub async fn enqueue_delivery(
    client: &Client,
    original: &StageOriginal<'_>,
    generation: i64,
    sender: &mut impl DeliverySender,
) -> Result<bool, DeliveryError> {
    let Some(delivery) = prepare_delivery(client, original, generation)
        .await
        .map_err(DeliveryError::Authority)?
    else {
        return Ok(false);
    };
    let encoded = delivery.encode().map_err(DeliveryError::Authority)?;
    sender
        .send(encoded)
        .await
        .map_err(|_| DeliveryError::Send)?;
    Ok(true)
}

/// Reload for every delivery (including redelivery), never execute an embedded
/// stage or blindly initialize a missing checkpoint. A parked/terminal current
/// generation needs no work; a generation invalidated by controls is stale.
/// Caller authenticates/opens the original first and applies its deadline.
pub async fn load_delivery(
    client: &Client,
    original: &StageOriginal<'_>,
    delivery: &Delivery,
) -> Result<Option<Snapshot>, StoreError> {
    if delivery.job_id() != original.job_id
        || delivery.idempotency_key() != original.idempotency_key
    {
        return Err(StoreError::Invalid);
    }
    let snapshot = load(client, original, delivery.generation()).await?;
    Ok(executable(&snapshot).then_some(snapshot))
}

/// Provider adapters expose fixed categories only; they cannot inject database,
/// stale-write or commit-uncertain errors into the service retry budget.
#[derive(Clone, Copy)]
pub enum ServiceError {
    Invalid,
    Transient { retry_after_secs: Option<u32> },
}

/// Replayable reads from the CC0 mirror and authorized probe service. The
/// authenticated original remains borrowed and no database transaction is open
/// during either call. Implementations must not log their inputs or raw errors.
pub trait StageServices {
    fn match_release(
        &mut self,
        parsed: &nntune_core::nzb::Document,
    ) -> impl std::future::Future<Output = Result<Option<String>, ServiceError>>;

    fn probe(
        &mut self,
        request: &crate::probe::ProbeRequest,
    ) -> impl std::future::Future<Output = Result<crate::probe::ProbeOutcome, ServiceError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStageOutcome {
    Inactive,
    Advanced(StageOutcome),
    ServiceFailure(ServiceFailureOutcome),
}

struct DeliveryRuntime<'a, 'doc, S> {
    client: &'a mut Client,
    original: &'a StageOriginal<'doc>,
    snapshot: Snapshot,
    services: &'a mut S,
    store_error: Option<StoreError>,
    retry_after_secs: Option<u32>,
}

impl<S: StageServices> DeliveryRuntime<'_, '_, S> {
    fn service_error(&mut self, error: ServiceError, stage: StageError) -> StageError {
        match error {
            ServiceError::Invalid => StageError::Invalid,
            ServiceError::Transient { retry_after_secs } => {
                self.retry_after_secs = retry_after_secs;
                stage
            }
        }
    }
}

impl<S: StageServices> StageRuntime for DeliveryRuntime<'_, '_, S> {
    async fn match_release(
        &mut self,
        parsed: &nntune_core::nzb::Document,
    ) -> Result<Option<String>, StageError> {
        self.services
            .match_release(parsed)
            .await
            .map_err(|error| self.service_error(error, StageError::Match))
    }

    async fn probe(
        &mut self,
        request: &crate::probe::ProbeRequest,
    ) -> Result<crate::probe::ProbeOutcome, StageError> {
        self.services
            .probe(request)
            .await
            .map_err(|error| self.service_error(error, StageError::Probe))
    }

    async fn persist(
        &mut self,
        expected: &JobCheckpoint,
        next: &JobCheckpoint,
    ) -> Result<bool, StageError> {
        if expected != self.snapshot.checkpoint() {
            self.store_error = Some(StoreError::Stale);
            return Ok(false);
        }
        match persist_stage(self.client, self.original, &self.snapshot, next).await {
            Ok(saved) => {
                self.snapshot = saved;
                Ok(true)
            }
            Err(error) => {
                self.store_error = Some(error);
                Err(StageError::Store)
            }
        }
    }
}

/// Compose one captured-generation delivery with the runner and real SQL CAS.
/// Reload before provider work; persist progress or typed transient failure
/// before returning. A retry result is a scheduling instruction, not a timer or
/// queue send. Redelivery reloads committed progress, never adopts a generation.
/// Database errors (including uncertain COMMIT) return unchanged as store errors
/// and must be resolved by authority reload, never by spending service attempts.
///
/// Caller authenticates/opens the original and applies a bounded deadline. No
/// live route, broker binding, recovery scanner or schema activation is installed
/// here. ReadyToPublish only validates saved evidence; it does not write an album.
pub async fn run_delivery_stage(
    client: &mut Client,
    original: &StageOriginal<'_>,
    delivery: &Delivery,
    services: &mut impl StageServices,
) -> Result<DeliveryStageOutcome, StoreError> {
    let Some(snapshot) = load_delivery(client, original, delivery).await? else {
        return Ok(DeliveryStageOutcome::Inactive);
    };
    let mut cp = snapshot.checkpoint().clone();
    let mut runtime = DeliveryRuntime {
        client,
        original,
        snapshot,
        services,
        store_error: None,
        retry_after_secs: None,
    };
    match crate::admin_execution::run_stage(
        &mut runtime,
        &mut cp,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .await
    {
        Ok(outcome) => Ok(DeliveryStageOutcome::Advanced(outcome)),
        Err(failure @ (StageError::Match | StageError::Probe)) => {
            let (_, outcome) = persist_service_failure(
                runtime.client,
                original,
                &runtime.snapshot,
                failure,
                runtime.retry_after_secs,
            )
            .await?;
            Ok(DeliveryStageOutcome::ServiceFailure(outcome))
        }
        Err(StageError::Invalid) => Err(StoreError::Invalid),
        Err(StageError::Store | StageError::Stale) => {
            Err(runtime.store_error.unwrap_or(StoreError::Invalid))
        }
    }
}

fn identity<'a>(original: &StageOriginal<'a>) -> Identity<'a> {
    Identity {
        job_id: original.job_id,
        user_id: "",
        idempotency_key: original.idempotency_key,
    }
}

// Bound the transferred JSON text before decoding. SQL NULL, JSON null and
// malformed/oversized checkpoints are all non-executable, not initializations.
// Original identity/length is rechecked after asynchronous provider reads.
const SELECT: &str = "SELECT j.status, j.admin_checkpoint_revision,
    j.admin_checkpoint IS NULL AS legacy_null,
    CASE WHEN octet_length(j.admin_checkpoint::text) <= 16384
         THEN j.admin_checkpoint::text ELSE NULL END AS checkpoint
    FROM ingest_jobs j JOIN ingest_originals o ON o.job_id = j.id
    WHERE j.id = $1 AND j.source = 'admin'
      AND j.idempotency_key = $2 AND j.content_sha256 = substring($2 from 7)
      AND j.nzb_bytes = $3 AND o.content_sha256 = j.content_sha256
      AND o.nzb_bytes = j.nzb_bytes
      AND o.nzb_ref_envelope_version = 2 AND o.nzb_ref_payload_format = 1
      AND octet_length(o.nzb_ref_encrypted) = $3 + 16
      AND octet_length(o.nzb_ref_dek_wrapped_nonce) = 12
      AND octet_length(o.nzb_ref_dek_wrapped) = 48
      AND octet_length(o.nzb_ref_payload_nonce) = 12 LIMIT 2";

async fn load_on(
    client: &impl GenericClient,
    original: &StageOriginal<'_>,
    generation: i64,
    lock: bool,
) -> Result<(Snapshot, String), StoreError> {
    if generation <= 0 {
        return Err(StoreError::Invalid);
    }
    let bytes = i64::try_from(original.nzb_bytes).map_err(|_| StoreError::Invalid)?;
    let sql = if lock {
        format!("{SELECT} FOR UPDATE OF j FOR SHARE OF o")
    } else {
        SELECT.into()
    };
    let rows = client
        .query(&sql, &[&original.job_id, &original.idempotency_key, &bytes])
        .await
        .map_err(|_| StoreError::Database)?;
    if rows.len() != 1 {
        return Err(StoreError::Stale);
    }
    let row = &rows[0];
    let encoded: Option<String> = row.try_get("checkpoint").map_err(|_| StoreError::Invalid)?;
    let encoded = encoded.ok_or(StoreError::Invalid)?;
    if encoded.len() > MAX_CHECKPOINT_BYTES {
        return Err(StoreError::Invalid);
    }
    let revision: i64 = row
        .try_get("admin_checkpoint_revision")
        .map_err(|_| StoreError::Invalid)?;
    let snapshot = Snapshot::decode(&encoded, revision, &identity(original))
        .map_err(|_| StoreError::Invalid)?;
    let status: String = row.try_get("status").map_err(|_| StoreError::Invalid)?;
    if status != snapshot.status() {
        return Err(StoreError::Invalid);
    }
    if snapshot.generation() != generation {
        return Err(StoreError::Stale);
    }
    Ok((snapshot, encoded))
}

/// Load only the generation captured by authenticated enqueue. Old deliveries
/// must not read the current generation and silently adopt a manual restart.
/// Terminal/parked snapshots may be inspected but cannot persist a stage.
pub async fn load(
    client: &Client,
    original: &StageOriginal<'_>,
    generation: i64,
) -> Result<Snapshot, StoreError> {
    load_on(client, original, generation, false)
        .await
        .map(|(snapshot, _)| snapshot)
}

/// Explicit reconciliation of historical SQL-NULL authority only. The caller
/// must authenticate the actor, supply the authenticated original, capture
/// expected status/revision, and apply its deadline. Role/deletion are rechecked
/// under the API's admin lock order; the redacted audit commits atomically. This
/// is not automatic repair, upload dedupe or a live route. Populated (even invalid)
/// checkpoints and parked/terminal rows need their separate legal control path.
/// All evidence starts fresh at Queued; revision/generation never reset. Enqueue
/// only after commit; commit-result uncertainty requires authority reload.
/// New uploads instead initialize with their job/original admission transaction.
/// This function never sends work or reads KV.
pub async fn reconcile_null(
    client: &mut Client,
    actor: ControlActor<'_>,
    original: &StageOriginal<'_>,
    expected_status: &str,
    expected_revision: i64,
) -> Result<Snapshot, StoreError> {
    actor.validate()?;
    if !matches!(expected_status, "queued" | "running") {
        return Err(StoreError::Invalid);
    }
    let successor = Snapshot::reconciled(&identity(original), expected_revision)
        .map_err(|_| StoreError::Invalid)?;
    let encoded = successor.encode().map_err(|_| StoreError::Invalid)?;
    let bytes = i64::try_from(original.nzb_bytes).map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    lock_actor(&tx, actor).await?;
    let rows = tx
        .query(
            &format!("{SELECT} FOR UPDATE OF j FOR SHARE OF o"),
            &[&original.job_id, &original.idempotency_key, &bytes],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if rows.len() != 1 {
        return Err(StoreError::Stale);
    }
    let row = &rows[0];
    let legacy_null: bool = row
        .try_get("legacy_null")
        .map_err(|_| StoreError::Invalid)?;
    let status: String = row.try_get("status").map_err(|_| StoreError::Invalid)?;
    let revision: i64 = row
        .try_get("admin_checkpoint_revision")
        .map_err(|_| StoreError::Invalid)?;
    if !legacy_null || status != expected_status || revision != expected_revision {
        return Err(StoreError::Stale);
    }
    let count = tx
        .execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $4::text::jsonb,
                admin_checkpoint_revision = $5, status = 'queued'
             WHERE id = $1 AND source = 'admin' AND status = $2
               AND admin_checkpoint_revision = $3 AND admin_checkpoint IS NULL",
            &[
                &original.job_id,
                &expected_status,
                &expected_revision,
                &encoded,
                &successor.revision(),
            ],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if count != 1 {
        return Err(StoreError::Stale);
    }
    audit_control(
        &tx,
        actor,
        "reconcile",
        expected_status,
        expected_revision,
        &successor,
    )
    .await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

/// Persist one ordinary stage and its status in a short job-locked transaction.
/// On stale/failed/cancelled writes the caller discards computed results. A
/// commit-result error is uncertain: reload DB, never blindly retry the write.
/// This function cannot initialize, restart, reject or publish a job.
pub async fn persist_stage(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    next: &JobCheckpoint,
) -> Result<Snapshot, StoreError> {
    let bound = identity(original);
    // Snapshot's public counter helper is not transition authority. Revalidate
    // both representations and derive the only permitted ordinary successor.
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    validate_stage_transition(
        expected.checkpoint(),
        next,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let successor = expected
        .successor(next.clone(), false, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

/// Persist a classified transient match/probe failure before scheduling retry.
/// Derive the next attempt (or review parking) rather than accepting caller state.
/// Full checkpoint/revision/generation comparison prevents duplicate settlement
/// and late failures from spending a newer attempt's budget. No raw error detail,
/// service call, queue send or retry sleep belongs in this transaction. On commit
/// uncertainty reload authority; never feed a database error back as a service
/// failure or blindly repeat this write. This installs no live consumer or timer.
pub async fn persist_service_failure(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    failure: StageError,
    retry_after_secs: Option<u32>,
) -> Result<(Snapshot, ServiceFailureOutcome), StoreError> {
    let bound = identity(original);
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    let (next, outcome) = service_failure_checkpoint(
        expected.checkpoint(),
        failure,
        retry_after_secs,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let successor = expected
        .successor(next, false, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok((successor, outcome))
}

/// Commit one explicit admin control with actor revalidation and atomic audit.
/// The caller authenticates the session (and archive capability for resume),
/// supplies its actor context, original holder and expected snapshot, and applies
/// its bounded database deadline. This is not a session-auth or live endpoint.
/// Rotate generation even on rejection so all old queue/service results lose.
/// Enqueue only the committed generation after success; commit errors require
/// authority reload, not blind replay. This function never sends queue work.
pub async fn apply_control(
    client: &mut Client,
    actor: ControlActor<'_>,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    control: Control<'_>,
) -> Result<Snapshot, StoreError> {
    actor.validate()?;
    let operation = match control {
        Control::ApproveReview { .. } => "approve",
        Control::RetryReview => "retry",
        Control::ResumeArchive => "resume_archive",
        Control::Reject => "reject",
    };
    let bound = identity(original);
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    let next = controlled_checkpoint(
        expected.checkpoint(),
        control,
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let successor = expected
        .successor(next, true, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    lock_actor(&tx, actor).await?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale);
    }
    update_on(&tx, expected, &current_encoded, &successor).await?;
    audit_control(
        &tx,
        actor,
        operation,
        expected.status(),
        expected.revision(),
        &successor,
    )
    .await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

async fn update_on(
    tx: &impl GenericClient,
    expected: &Snapshot,
    current_encoded: &str,
    successor: &Snapshot,
) -> Result<(), StoreError> {
    // After strict whole-snapshot comparison, retain the selected representation
    // for SQL equality: optional null vs omitted fields need not round-trip to
    // the same JSON keys. Revision prevents ABA when checkpoint contents repeat.
    let next_encoded = successor.encode().map_err(|_| StoreError::Invalid)?;
    let count = tx
        .execute(
            "UPDATE ingest_jobs SET admin_checkpoint = $5::text::jsonb,
                admin_checkpoint_revision = $6, status = $7
             WHERE id = $1 AND source = 'admin' AND status = $2
               AND admin_checkpoint_revision = $3
               AND admin_checkpoint = $4::text::jsonb",
            &[
                &expected.checkpoint().job_id,
                &expected.status(),
                &expected.revision(),
                &current_encoded,
                &next_encoded,
                &successor.revision(),
                &successor.status(),
            ],
        )
        .await
        .map_err(|_| StoreError::Database)?;
    if count != 1 {
        return Err(StoreError::Stale);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationError {
    Checkpoint(StoreError),
    Album(WriteError),
}

impl From<StoreError> for PublicationError {
    fn from(error: StoreError) -> Self {
        Self::Checkpoint(error)
    }
}

/// Commit album rows and Done in one outer transaction, never merely release
/// the album savepoint. The caller has already authenticated the original and
/// obtained CC0 mirror recordings and trusted decoded-file geometry; no network
/// work belongs under this lock. The sealer is local API/keyring work. The caller
/// applies its bounded deadline to the whole future, including sealing/commit.
/// Any error/drop before commit rolls back all rows. A commit-result error is
/// uncertain: reload authority before deciding whether publication succeeded.
pub async fn publish(
    client: &mut Client,
    original: &StageOriginal<'_>,
    expected: &Snapshot,
    recordings: &[MbRecording],
    geometry: &[FileGeometry],
    sealer: &mut impl SourceSealer,
) -> Result<Snapshot, PublicationError> {
    let bound = identity(original);
    let encoded = expected.encode().map_err(|_| StoreError::Invalid)?;
    Snapshot::decode(&encoded, expected.revision(), &bound).map_err(|_| StoreError::Invalid)?;
    validate_publication(
        expected.checkpoint(),
        original.job_id,
        original.idempotency_key,
        original.parsed,
    )
    .map_err(|_| StoreError::Invalid)?;
    let mut done = expected.checkpoint().clone();
    if jobs::advance_checkpoint(&mut done, false) != JobState::Done {
        return Err(StoreError::Invalid.into());
    }
    let successor = expected
        .successor(done, false, &bound)
        .map_err(|_| StoreError::Invalid)?;
    let mut tx = client
        .transaction()
        .await
        .map_err(|_| StoreError::Database)?;
    let (current, current_encoded) = load_on(&tx, original, expected.generation(), true).await?;
    if current != *expected {
        return Err(StoreError::Stale.into());
    }
    write_album(
        &mut tx,
        sealer,
        &AlbumWrite {
            checkpoint: current.checkpoint(),
            original_job: original.job_id,
            original_key: original.idempotency_key,
            parsed: original.parsed,
            recordings,
            geometry,
        },
    )
    .await
    .map_err(PublicationError::Album)?;
    update_on(&tx, expected, &current_encoded, &successor).await?;
    tx.commit().await.map_err(|_| StoreError::Database)?;
    Ok(successor)
}

#[cfg(test)]
#[path = "admin_delivery_tests.rs"]
mod delivery_tests;
