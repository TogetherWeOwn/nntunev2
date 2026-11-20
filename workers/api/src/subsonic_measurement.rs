//! Default-off staging fixture mount. There is no catalogue DB connector here.

use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection, StagingRejection::*};
use worker::js_sys::{Function, JsString};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{web_sys, Env, Response, Result as WorkerResult};
use zeroize::Zeroizing;

use crate::handlers::qa_bearer_ok;
use crate::subsonic_experiment_gate::{Deployment, Lease};
use crate::subsonic_request_metadata::{is_ascii_literal, property, request_url};
use crate::subsonic_staging::respond_fixed_rejection;
use crate::worker_entry::with_deadline;

use self::schedule::Schedule;

#[cfg(not(feature = "staging-body-wasm-test"))]
mod connection;
mod schedule;

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|value| value.to_string())
}

fn admitted<'a>(
    request: &web_sys::Request,
    env: &Env,
    now: u64,
    raw: &'a str,
) -> Option<(Lease<'a>, Schedule)> {
    if var(env, "SUBSONIC_MEASUREMENT_ENABLE").as_deref() != Some("1")
        || var(env, "SUBSONIC_MEASUREMENT_STAGE").as_deref() != Some("staging")
    {
        return None;
    }
    let stage_host = var(env, "RP_ID")?;
    let sha = var(env, "BUILD_SHA")?;
    let url = request_url(request).ok()?;
    let path: JsString = property(&url, "pathname").ok()?;
    if !is_ascii_literal(&path, "/rest") && !path.starts_with("/rest/", 0) {
        return None;
    }
    let scheme: JsString = property(&url, "protocol").ok()?;
    let host: JsString = property(&url, "host").ok()?;
    let (lease, cases) = (raw.len() <= 640).then_some(raw)?.split_once(';')?;
    let lease = Lease::admit(
        &Deployment {
            is_staging: true,
            staging_host: &stage_host,
            revision: &sha,
            now_utc_seconds: now,
        },
        (is_ascii_literal(&scheme, "https:") && is_ascii_literal(&host, &stage_host))
            .then_some(stage_host.as_str()),
        Some("1"),
        Some(lease),
    )?;
    Some((lease, Schedule::parse(cases)?))
}

fn authorized_runner(request: &web_sys::Request, env: &Env) -> Option<()> {
    (var(env, "QA_CODE_SEAM").as_deref() == Some("1")).then_some(())?;
    let token = Zeroizing::new(env.secret("QA_AUTH_TOKEN").ok()?.to_string());
    if token.trim().is_empty() || token.len() > 1024 {
        return None;
    }
    // Match the QA bearer without copying an unbounded header into Rust.
    let value = header(request, "authorization")?;
    if value.length() as usize != "Bearer ".len() + token.len() {
        return None;
    }
    let value = Zeroizing::new(String::from(value));
    qa_bearer_ok(value.strip_prefix("Bearer "), &token).then_some(())
}

fn header(request: &web_sys::Request, name: &str) -> Option<JsString> {
    let headers = request.headers();
    let get: Function = property(headers.as_ref(), "get").ok()?;
    get.call1(headers.as_ref(), &JsValue::from_str(name))
        .ok()?
        .dyn_into()
        .ok()
}

fn runner_case(
    request: &web_sys::Request,
    env: &Env,
    schedule: Schedule,
) -> Option<schedule::Case> {
    authorized_runner(request, env)?;
    // Match only a scheduled literal in JS; do not copy selector values into Rust.
    schedule.resolve(&header(request, "x-subsonic-measurement-case")?)
}

async fn within_request(
    operation: impl std::future::Future<Output = WorkerResult<Response>>,
    timeout_ms: u64,
) -> WorkerResult<Response> {
    with_deadline(async { Ok(operation.await) }, timeout_ms, BusyOrCapacity)
        .await
        .unwrap_or_else(reject)
}

fn reject(reason: StagingRejection) -> WorkerResult<Response> {
    respond_fixed_rejection(reason, RejectionFormat::Xml)
}

#[cfg(not(feature = "staging-body-wasm-test"))]
pub(super) async fn mount(request: &web_sys::Request, env: &Env) -> Option<WorkerResult<Response>> {
    let raw = var(env, "SUBSONIC_MEASUREMENT_LEASE")?;
    let now_ms = worker::Date::now().as_millis();
    let (lease, schedule) = admitted(request, env, now_ms / 1000, &raw)?;
    let Some(case) = runner_case(request, env, schedule) else {
        return Some(crate::subsonic_staging_response::finish_document(Ok(None)));
    };
    Some(connection::respond(request, env, lease, case, now_ms).await)
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use worker::wasm_bindgen::{self, prelude::*};

    #[wasm_bindgen]
    pub fn staging_measurement_admits(request: web_sys::Request, env: Env, now: u32) -> bool {
        var(&env, "SUBSONIC_MEASUREMENT_LEASE").is_some_and(|raw| {
            admitted(&request, &env, u64::from(now), &raw)
                .and_then(|(_, schedule)| runner_case(&request, &env, schedule))
                .and_then(schedule::Case::catalogue)
                .is_some()
        })
    }
}
