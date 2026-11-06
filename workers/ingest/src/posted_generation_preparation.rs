//! Trusted per-file continuation and concrete private-plan/R2/Queue adapters.
//! Source-only and default-off: no public route, new admission, posting, cleanup
//! or old-consumer fallback. Effective service grants remain deployment work.

use super::*;
use nntune_core::preparation_plan::PlanBytes;
use nntune_core::preparation_plan::{self, PlanReference, PreparationPlan, PLAN_BYTE_CAP};

pub const PLAN_SERVICE_BINDING: &str = "API_PREPARATION_PLANS";
const MODE_VAR: &str = "GENERATION_PREPARATION_MODE";

pub trait PlanService {
    fn freeze<'a>(
        &'a mut self,
        canonical: &'a [u8],
    ) -> impl Future<Output = Result<PlanReference, StoreError>> + 'a;
    fn open<'a>(
        &'a mut self,
        pointer: &'a DispatchPointer,
        reference: &'a PlanReference,
    ) -> impl Future<Output = Result<PlanBytes, StoreError>> + 'a;
}

/// Real platform Fetcher, never an Internet URL or public API client.
pub struct ApiPlanService {
    fetcher: worker::Fetcher,
}
impl ApiPlanService {
    pub fn from_env(env: &worker::Env) -> Result<Self, StoreError> {
        if mode(env)? != DispatchMode::PreparationOnly {
            return Err(StoreError::Inconsistent);
        }
        Ok(Self {
            fetcher: env
                .service(PLAN_SERVICE_BINDING)
                .map_err(|_| StoreError::Inconsistent)?,
        })
    }

    async fn call(&self, frame: &[u8], cap: usize) -> Result<PlanBytes, StoreError> {
        use futures_util::StreamExt;
        // Request URL satisfies Fetcher syntax only; it selects no authority.
        // Manual redirects prevent a mistaken default target from forwarding.
        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post)
            .with_redirect(worker::RequestRedirect::Manual)
            .with_body(Some(worker::js_sys::Uint8Array::from(frame).into()));
        let req = worker::Request::new_with_init("https://preparation.invalid/", &init)
            .map_err(|_| StoreError::InvalidInput)?;
        let mut response = self
            .fetcher
            .fetch_request(req)
            .await
            .map_err(|_| StoreError::Database)?;
        if response.status_code() != 200
            || response
                .headers()
                .get("Content-Type")
                .map_err(|_| StoreError::Inconsistent)?
                .as_deref()
                != Some("application/octet-stream")
        {
            return Err(StoreError::Inconsistent);
        }
        let mut stream = response.stream().map_err(|_| StoreError::Inconsistent)?;
        let mut bytes = PlanBytes::new(Vec::new());
        while let Some(chunk) = stream.next().await {
            let chunk = PlanBytes::new(chunk.map_err(|_| StoreError::Database)?);
            if chunk.len() > cap.saturating_sub(bytes.len()) {
                return Err(StoreError::Inconsistent);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }
}

impl PlanService for ApiPlanService {
    async fn freeze(&mut self, canonical: &[u8]) -> Result<PlanReference, StoreError> {
        if canonical.is_empty() || canonical.len() > PLAN_BYTE_CAP {
            return Err(StoreError::InvalidInput);
        }
        PreparationPlan::decode(canonical).map_err(|_| StoreError::InvalidInput)?;
        let mut frame = PlanBytes::new(vec![preparation_plan::FREEZE]);
        frame.extend_from_slice(canonical);
        let bytes = self.call(&frame, 167).await?;
        let reference = PlanReference::decode(&bytes).map_err(|_| StoreError::Inconsistent)?;
        if reference.bytes as usize != canonical.len()
            || reference.digest != <[u8; 32]>::from(Sha256::digest(canonical))
        {
            return Err(StoreError::Inconsistent);
        }
        Ok(reference)
    }

    async fn open(
        &mut self,
        pointer: &DispatchPointer,
        reference: &PlanReference,
    ) -> Result<PlanBytes, StoreError> {
        let frame = reference
            .open_request(&pointer.id)
            .map_err(|_| StoreError::InvalidInput)?;
        let bytes = self.call(&frame, PLAN_BYTE_CAP).await?;
        if bytes.len() != reference.bytes as usize
            || <[u8; 32]>::from(Sha256::digest(bytes.as_slice())) != reference.digest
        {
            return Err(StoreError::Inconsistent);
        }
        PreparationPlan::decode(&bytes).map_err(|_| StoreError::Inconsistent)?;
        Ok(bytes)
    }
}

fn digest(raw: &str) -> Result<[u8; 32], StoreError> {
    hex::decode(raw)
        .map_err(|_| StoreError::Inconsistent)?
        .try_into()
        .map_err(|_| StoreError::Inconsistent)
}

/// At most one opened bounded plan; only the exact separate plan reference can
/// select API custody. Tagged ranges can never alias plan storage. Drop/EOF
/// wipes the buffer, including cancellation and errors.
pub struct CompositeReader<'a, S: storage::TaggedObjectStore, P: PlanService> {
    tagged: storage::TaggedFileReader<'a, S>,
    file: PreparedDispatchFile,
    pointer: DispatchPointer,
    plans: &'a mut P,
    opened: Option<PlanBytes>,
}
impl<'a, S: storage::TaggedObjectStore, P: PlanService> CompositeReader<'a, S, P> {
    pub fn new(
        store: &'a mut S,
        plans: &'a mut P,
        pointer: DispatchPointer,
        file: PreparedDispatchFile,
    ) -> Result<Self, StoreError> {
        if file.object == file.plan
            || file.plan_bytes <= 0
            || file.plan_bytes > PLAN_BYTE_CAP as i64
        {
            return Err(StoreError::InvalidInput);
        }
        Ok(Self {
            tagged: storage::TaggedFileReader::new(
                store,
                file.object.clone(),
                u64::try_from(file.plaintext_bytes).map_err(|_| StoreError::InvalidInput)?,
            )?,
            file,
            pointer,
            plans,
            opened: None,
        })
    }
}
impl<S: storage::TaggedObjectStore, P: PlanService> DispatchObjectReader
    for CompositeReader<'_, S, P>
{
    async fn read(
        &mut self,
        object: &DispatchObject,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>, StoreError> {
        if object == &self.file.object {
            return self.tagged.read(object, offset, limit).await;
        }
        if object != &self.file.plan
            || limit == 0
            || limit > 65_536
            || offset > self.file.plan_bytes as u64
        {
            self.opened = None;
            return Err(StoreError::InvalidInput);
        }
        if offset == self.file.plan_bytes as u64 {
            self.opened = None;
            return Ok(Vec::new());
        }
        if self.opened.is_none() {
            let reference = PlanReference {
                id: self.file.plan.id.clone(),
                version: self.file.plan.version.clone(),
                bytes: self.file.plan_bytes as u32,
                digest: digest(&self.file.plan_sha256)?,
            };
            let bytes = self.plans.open(&self.pointer, &reference).await?;
            let plan = PreparationPlan::decode(&bytes).map_err(|_| StoreError::Inconsistent)?;
            if bytes.len() != reference.bytes as usize
                || <[u8; 32]>::from(Sha256::digest(bytes.as_slice())) != reference.digest
                || plan.object != self.file.object.id
                || plan.object_version != self.file.object.version
                || plan.file_index as i32 != self.file.file_index
                || plan.plaintext_bytes != self.file.plaintext_bytes as u64
                || plan.plaintext_digest != digest(&self.file.plaintext_sha256)?
            {
                return Err(StoreError::Inconsistent);
            }
            self.opened = Some(bytes);
        }
        let bytes = self.opened.as_ref().ok_or(StoreError::Inconsistent)?;
        let start = offset as usize;
        Ok(bytes[start..bytes.len().min(start + limit)].to_vec())
    }
}

pub struct IngestDispatchQueue {
    queue: worker::Queue,
}
impl IngestDispatchQueue {
    pub fn from_env(env: &worker::Env) -> Result<Self, StoreError> {
        Ok(Self {
            queue: env
                .queue(crate::dispatch::INGEST_QUEUE_BINDING)
                .map_err(|_| StoreError::Inconsistent)?,
        })
    }
}
impl DispatchQueue for IngestDispatchQueue {
    async fn send(&mut self, pointer: DispatchPointer) -> Result<(), StoreError> {
        self.queue
            .send(pointer)
            .await
            .map_err(|_| StoreError::Database)
    }
}

pub fn mode(env: &worker::Env) -> Result<DispatchMode, StoreError> {
    let value = worker::js_sys::Reflect::get(
        env.as_ref(),
        &worker::wasm_bindgen::JsValue::from_str(MODE_VAR),
    )
    .map_err(|_| StoreError::InvalidInput)?;
    mode_from_binding(!value.is_undefined(), value.as_string().as_deref())
}

fn mode_from_binding(present: bool, value: Option<&str>) -> Result<DispatchMode, StoreError> {
    // Present object/number/null and lookup faults are not absence. In
    // particular they must not fall back to the old Queue consumer.
    if present && value.is_none() {
        return Err(StoreError::InvalidInput);
    }
    preparation_plan::preparation_enabled(value)
        .map(|enabled| {
            if enabled {
                DispatchMode::PreparationOnly
            } else {
                DispatchMode::Disabled
            }
        })
        .map_err(|_| StoreError::InvalidInput)
}

fn prerequisites(env: &worker::Env) -> Result<(), StoreError> {
    // Same explicit uncached-read attestation used at the API boundary.
    if env
        .var("HYPERDRIVE_CACHING_DISABLED")
        .ok()
        .map(|v| v.to_string())
        .as_deref()
        != Some("1")
    {
        return Err(StoreError::Inconsistent);
    }
    env.hyperdrive(crate::library::LIBRARY_DB_BINDING)
        .map_err(|_| StoreError::Inconsistent)?;
    Ok(())
}

/// Explicit trusted continuation inputs; no defaults and no Queue provenance.
/// The business phase must supply its approved blocks-per-article policy.
pub struct PreparationInput<'a> {
    pub job: &'a str,
    pub operation: &'a str,
    pub predecessor: i64,
    pub file_index: i32,
    pub blocks_per_article: u32,
    pub tagged: crate::flac::TaggedFlac,
}

/// Complete production adapter composition. It is not a public business-phase
/// entrypoint; no route or old consumer invokes it. Retry ambiguous outcomes
/// with this SAME job/operation, retaining R2 and the sealed DB envelope.
pub async fn prepare_and_send(
    env: &worker::Env,
    input: PreparationInput<'_>,
) -> Result<DispatchSend, StoreError> {
    if mode(env)? == DispatchMode::Disabled {
        return Ok(DispatchSend::Disabled);
    }
    prerequisites(env)?;
    let mut store = storage::R2TaggedObjectStore::from_env(env)?;
    let mut plans = ApiPlanService::from_env(env)?;
    let mut queue = IngestDispatchQueue::from_env(env)?;
    let trace = nntune_logging::resolve_trace_id(None);
    let mut client = crate::library_db_client(env, &trace)
        .await
        .map_err(|_| StoreError::Database)?;
    bounded(async {
        let Some(pointer) = prepare(&mut client, input, &mut store, &mut plans).await? else {
            return Ok(DispatchSend::StaleOrMissing);
        };
        let tx = client.transaction().await.map_err(database_error)?;
        let Some(row) = dispatch_locked(&tx, &pointer).await? else {
            return Ok(DispatchSend::StaleOrMissing);
        };
        let file = PreparedDispatchFile::from_row(&row);
        tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
        let mut reader = CompositeReader::new(&mut store, &mut plans, pointer.clone(), file)?;
        send_file_dispatch(
            &mut client,
            DispatchMode::PreparationOnly,
            &pointer,
            &mut reader,
            &mut queue,
        )
        .await
    })
    .await
}

/// Internal composition accepts injected IO for the disposable-DB bank, but the
/// runtime above supplies actual platform adapters. One tagged PUT, never two.
pub async fn prepare<S: storage::TaggedObjectStore, P: PlanService>(
    client: &mut Client,
    input: PreparationInput<'_>,
    store: &mut S,
    plans: &mut P,
) -> Result<Option<DispatchPointer>, StoreError> {
    if input.predecessor < 0
        || input.predecessor == i64::MAX
        || input.blocks_per_article == 0
        || input.file_index < 0
        || !identity(input.job)
        || !identity(input.operation)
    {
        return Err(StoreError::InvalidInput);
    }
    let tx = client.transaction().await.map_err(database_error)?;
    let Some(job) = admission_locked(&tx, input.job, None).await? else {
        return Ok(None);
    };
    let provenance = AdmissionIdentity::from_row(&job);
    if input.file_index >= provenance.file_count {
        return Ok(None);
    }
    let predecessor_ok: bool = tx.query_one(
        "SELECT COALESCE((SELECT attempt FROM posted_file_job_attempts WHERE job_id = $1),0) = $3 \
         OR EXISTS (SELECT 1 FROM ingest_file_dispatches WHERE job_id = $1 AND operation_key = $2 \
         AND expected_attempt = $3)",
        &[&input.job, &input.operation, &input.predecessor],
    ).await.map_err(database_error)?.get(0);
    if !predecessor_ok {
        return Ok(None);
    }
    tx.commit().await.map_err(|_| StoreError::CommitUnknown)?;
    let frozen = storage::freeze_tagged_file(
        store,
        input.job,
        input.operation,
        input.file_index,
        input.tagged,
    )
    .await?;
    let plan = PreparationPlan {
        owner: provenance.owner.clone(),
        job: input.job.to_owned(),
        operation: input.operation.to_owned(),
        predecessor: input.predecessor as u64,
        original_digest: digest(&provenance.original)?,
        original_bytes: provenance.original_bytes as u64,
        file_count: provenance.file_count as u32,
        file_index: input.file_index as u32,
        object: frozen.object.id.clone(),
        object_version: frozen.object.version.clone(),
        plaintext_bytes: frozen.size as u64,
        plaintext_digest: digest(&frozen.sha256)?,
        blocks_per_article: input.blocks_per_article,
    };
    let canonical = PlanBytes::new(plan.encode().map_err(|_| StoreError::InvalidInput)?);
    let reference = plans.freeze(&canonical).await?;
    if reference.bytes as usize != canonical.len()
        || reference.digest != <[u8; 32]>::from(Sha256::digest(canonical.as_slice()))
    {
        return Err(StoreError::Inconsistent);
    }
    let file = PreparedDispatchFile {
        file_index: input.file_index,
        object: frozen.object,
        plaintext_bytes: frozen.size,
        plaintext_sha256: frozen.sha256,
        plan: DispatchObject::new(&reference.id, &reference.version)?,
        plan_bytes: canonical.len() as i64,
        plan_sha256: hex::encode(reference.digest),
    };
    prepare_file_dispatch_bound(
        client,
        DispatchMode::PreparationOnly,
        input.job,
        input.operation,
        input.predecessor,
        &file,
        Some(&provenance),
    )
    .await
}

/// Selected generation branch in the real Queue handler. It can only recover an
/// already acquired exact receipt before/after external reads. Never start/send.
pub async fn recover_delivery(
    env: &worker::Env,
    pointer: &DispatchPointer,
) -> Result<(), StoreError> {
    if mode(env)? != DispatchMode::PreparationOnly {
        return Err(StoreError::Inconsistent);
    }
    prerequisites(env)?;
    let mut store = storage::R2TaggedObjectStore::from_env(env)?;
    let mut plans = ApiPlanService::from_env(env)?;
    // Require the complete selected deployment, even on recovery-only entries.
    let _queue = IngestDispatchQueue::from_env(env)?;
    let trace = nntune_logging::resolve_trace_id(None);
    let mut client = crate::library_db_client(env, &trace)
        .await
        .map_err(|_| StoreError::Database)?;
    bounded(async {
        let Some((_, file)) = recover_pointer(&mut client, pointer).await? else {
            return Err(StoreError::Inconsistent);
        };
        let mut reader = CompositeReader::new(&mut store, &mut plans, pointer.clone(), file)?;
        match recover_file_dispatch(
            &mut client,
            DispatchMode::PreparationOnly,
            pointer,
            &mut reader,
        )
        .await?
        {
            AttemptOutcome::Acquired(_) => Ok(()),
            _ => Err(StoreError::Inconsistent),
        }
    })
    .await
}

async fn bounded<T>(
    operation: impl Future<Output = Result<T, StoreError>>,
) -> Result<T, StoreError> {
    crate::library::with_db_deadline(
        operation,
        worker::Delay::from(std::time::Duration::from_millis(
            crate::library::LIBRARY_DB_DEADLINE_MS,
        )),
        || StoreError::Database,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_actual_absence_or_disabled_selects_disabled_mode() {
        assert_eq!(mode_from_binding(false, None), Ok(DispatchMode::Disabled));
        assert_eq!(
            mode_from_binding(true, Some("disabled")),
            Ok(DispatchMode::Disabled)
        );
        assert_eq!(
            mode_from_binding(true, Some("preparation_only")),
            Ok(DispatchMode::PreparationOnly)
        );
        assert_eq!(
            mode_from_binding(true, Some("unknown")),
            Err(StoreError::InvalidInput)
        );
        assert_eq!(mode_from_binding(true, None), Err(StoreError::InvalidInput));
    }
}
