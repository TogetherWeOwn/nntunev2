//! Private named-entry handler. Never called by the public API router.
//! The platform binding is the caller capability, not a URL/header/session.
//! Preparation is inert and default-off; no startup/acquisition or KEK response.

use crate::{
    ingest_plan::{self, PlanError, PlanMode, PlanRequest},
    rotation::KekSource,
};
use nntune_core::preparation_plan::{PlanReference, PreparationPlan, FREEZE, OPEN, PLAN_BYTE_CAP};
use sha2::{Digest, Sha256};
use tokio_postgres::Client;
use zeroize::Zeroizing;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// Derive provenance from the DB, then compare the complete submitted relation.
// freeze/read independently repeat admission locks in the established order.
async fn admitted_plan(client: &Client, plan: &PreparationPlan) -> Result<(), PlanError> {
    let row = client
        .query_opt(
            "SELECT user_id, content_sha256, nzb_bytes, file_count FROM ingest_jobs WHERE id = $1 \
         AND source = 'admin' AND status IN ('queued','running') AND failure_reason IS NULL",
            &[&plan.job],
        )
        .await
        .map_err(|_| PlanError::Store)?
        .ok_or(PlanError::Conflict)?;
    if row.get::<_, String>("user_id") != plan.owner
        || row.get::<_, String>("content_sha256") != hex(&plan.original_digest)
        || row.get::<_, i64>("nzb_bytes") as u64 != plan.original_bytes
        || row.get::<_, i32>("file_count") as u32 != plan.file_count
    {
        return Err(PlanError::Conflict);
    }
    Ok(())
}

fn request<'a>(plan: &'a PreparationPlan, original: &'a str) -> PlanRequest<'a> {
    PlanRequest {
        owner_id: &plan.owner,
        job_id: &plan.job,
        operation_key: &plan.operation,
        file_index: plan.file_index as i32,
        original_sha256: original,
        original_bytes: plan.original_bytes as i64,
        file_count: plan.file_count as i32,
    }
}

/// The only private wire protocol: one-byte freeze + canonical bytes, or exact
/// binary open reference. Extra fields/trailing bytes cannot be ignored.
/// Output stays in bounded wiping Rust memory until the private response copy.
pub async fn process(
    client: &mut Client,
    mode: PlanMode,
    keks: &impl KekSource,
    kek_version: u32,
    frame: &[u8],
) -> Result<Zeroizing<Vec<u8>>, PlanError> {
    if mode != PlanMode::PreparationOnly || frame.len() > PLAN_BYTE_CAP + 1 {
        return Err(PlanError::Invalid);
    }
    match frame.first().copied() {
        Some(FREEZE) => {
            let canonical = &frame[1..];
            let plan = PreparationPlan::decode(canonical).map_err(|_| PlanError::Invalid)?;
            admitted_plan(client, &plan).await?;
            let original = hex(&plan.original_digest);
            let opened = ingest_plan::freeze_plan(
                client,
                mode,
                keks,
                kek_version,
                &request(&plan, &original),
                canonical,
            )
            .await?
            .ok_or(PlanError::Conflict)?;
            let reference = PlanReference {
                id: opened.id().to_owned(),
                version: opened.version().to_owned(),
                digest: Sha256::digest(opened.bytes()).into(),
                bytes: opened.bytes().len() as u32,
            };
            Ok(Zeroizing::new(
                reference.encode().map_err(|_| PlanError::Invalid)?,
            ))
        }
        Some(OPEN) => {
            let (dispatch, reference) =
                PlanReference::decode_open(frame).map_err(|_| PlanError::Invalid)?;
            let row = client
                .query_opt(
                    "SELECT d.*, j.nzb_bytes, j.file_count FROM ingest_file_dispatches d \
                 JOIN ingest_jobs j ON j.id = d.job_id WHERE d.id = $1 AND d.plan_id = $2 \
                 AND d.plan_version = $3 AND d.plan_format = 1 AND d.plan_bytes = $4 \
                 AND d.plan_sha256 = $5 AND d.purpose = 'prepare'",
                    &[
                        &dispatch,
                        &reference.id,
                        &reference.version,
                        &i64::from(reference.bytes),
                        &hex(&reference.digest),
                    ],
                )
                .await
                .map_err(|_| PlanError::Store)?
                .ok_or(PlanError::Conflict)?;
            let owner: String = row.get("owner_id");
            let job: String = row.get("job_id");
            let operation: String = row.get("operation_key");
            let original: String = row.get("original_sha256");
            let input = PlanRequest {
                owner_id: &owner,
                job_id: &job,
                operation_key: &operation,
                file_index: row.get("file_index"),
                original_sha256: &original,
                original_bytes: row.get("nzb_bytes"),
                file_count: row.get("file_count"),
            };
            let opened = ingest_plan::read_plan(
                client,
                mode,
                keks,
                &input,
                &reference.id,
                &reference.version,
            )
            .await?
            .ok_or(PlanError::Conflict)?;
            let canonical = opened.bytes();
            if canonical.len() != reference.bytes as usize
                || <[u8; 32]>::from(Sha256::digest(canonical)) != reference.digest
            {
                return Err(PlanError::Conflict);
            }
            let plan = PreparationPlan::decode(canonical).map_err(|_| PlanError::Invalid)?;
            admitted_plan(client, &plan).await?;
            if plan.owner != owner
                || plan.job != job
                || plan.operation != operation
                || hex(&plan.original_digest) != original
                || plan.file_index as i32 != input.file_index
                || plan.original_bytes as i64 != input.original_bytes
                || plan.file_count as i32 != input.file_count
                || plan.predecessor != row.get::<_, i64>("expected_attempt") as u64
                || plan.object != row.get::<_, String>("object_id")
                || plan.object_version != row.get::<_, String>("object_version")
                || plan.plaintext_bytes != row.get::<_, i64>("plaintext_bytes") as u64
                || hex(&plan.plaintext_digest) != row.get::<_, String>("plaintext_sha256")
            {
                return Err(PlanError::Conflict);
            }
            Ok(Zeroizing::new(canonical.to_vec()))
        }
        _ => Err(PlanError::Invalid),
    }
}

#[cfg(all(target_arch = "wasm32", feature = "preparation-service"))]
async fn handle(mut req: worker::Request, env: worker::Env) -> worker::Result<worker::Response> {
    use crate::worker_entry::{config_from_env, preparation_client, with_deadline};
    use futures_util::StreamExt;
    use nntune_core::preparation_plan::preparation_enabled;
    let enabled = env
        .var("GENERATION_PREPARATION_MODE")
        .ok()
        .map(|v| v.to_string());
    // No URL/header can select this function from public fetch.
    if preparation_enabled(enabled.as_deref()) != Ok(true) || req.method() != worker::Method::Post {
        return worker::Response::error("preparation unavailable", 503);
    }
    let Some(config) = config_from_env(&env) else {
        return worker::Response::error("preparation unavailable", 503);
    };
    let result = with_deadline(
        async {
            let mut stream = req.stream().map_err(|_| PlanError::Invalid)?;
            let mut frame = Zeroizing::new(Vec::new());
            while let Some(chunk) = stream.next().await {
                let chunk = Zeroizing::new(chunk.map_err(|_| PlanError::Invalid)?);
                if chunk.len() > (PLAN_BYTE_CAP + 1).saturating_sub(frame.len()) {
                    return Err(PlanError::Invalid);
                }
                frame.extend_from_slice(&chunk);
            }
            let mut client = preparation_client(env)
                .await
                .map_err(|_| PlanError::Store)?;
            process(
                &mut client,
                PlanMode::PreparationOnly,
                &config,
                config.kek_version,
                &frame,
            )
            .await
        },
        crate::vault::TOTAL_TIMEOUT_MS,
        PlanError::Store,
    )
    .await;
    match result {
        Ok(bytes) => {
            let mut response = worker::Response::from_bytes(bytes.to_vec())?;
            response
                .headers_mut()
                .set("Content-Type", "application/octet-stream")?;
            response.headers_mut().set("Cache-Control", "no-store")?;
            Ok(response)
        }
        Err(_) => worker::Response::error("preparation unavailable", 503),
    }
}

/// Ordinary Wasm bridge, not a second fetch event or public route. Only the
/// outer named facade may delegate here once its generated ABI is qualified.
#[cfg(all(target_arch = "wasm32", feature = "preparation-service"))]
#[worker::wasm_bindgen::prelude::wasm_bindgen(wasm_bindgen = worker::wasm_bindgen, wasm_bindgen_futures = worker::wasm_bindgen_futures, js_sys = worker::js_sys)]
pub async fn ingest_preparation_plan(
    req: worker::web_sys::Request,
    env: worker::Env,
) -> Result<worker::web_sys::Response, worker::wasm_bindgen::JsValue> {
    handle(req.into(), env)
        .await
        .map(Into::into)
        .map_err(|_| worker::wasm_bindgen::JsValue::from_str("preparation unavailable"))
}
