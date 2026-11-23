//! Default-off staging fixture mount. There is no catalogue DB connector here.

use nntune_subsonic::staging_rejection::{RejectionFormat, StagingRejection, StagingRejection::*};
use subtle::ConstantTimeEq;
use worker::js_sys::{Function, JsString};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::{web_sys, Env, Response, Result as WorkerResult};
use zeroize::Zeroizing;

use crate::subsonic_experiment_gate::{Deployment, Lease};
use crate::subsonic_request_metadata::{is_ascii_literal, property, request_url};
use crate::subsonic_staging::respond_fixed_rejection;
use crate::worker_entry::with_deadline;

use self::schedule::Schedule;

#[cfg(not(feature = "staging-body-wasm-test"))]
mod connection;
mod schedule;

#[cfg(all(
    feature = "billing-owned-wasm-test",
    not(feature = "staging-body-wasm-test")
))]
pub(crate) use connection::test_connect_deadline;

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
    let method: JsString = property(request.as_ref(), "method").ok()?;
    if !is_ascii_literal(&method, "POST") {
        return None;
    }
    let url = request_url(request).ok()?;
    let path: JsString = property(&url, "pathname").ok()?;
    if !["/rest/getArtists", "/rest/getArtist", "/rest/getAlbum"]
        .iter()
        .any(|literal| is_ascii_literal(&path, literal))
    {
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

// Store the bearer and its UTC window atomically, independently of QA auth.
const MAX_RUNNER_AGE_SECONDS: u64 = 30 * 24 * 60 * 60;
const RUNNER_BEARER_LEN: usize = 43; // Unpadded base64url of 32 random bytes.

fn authorized_runner(request: &web_sys::Request, env: &Env, now: u64) -> Option<()> {
    (var(env, "QA_CODE_SEAM").as_deref() == Some("1")).then_some(())?;
    let binding = Zeroizing::new(env.secret("SUBSONIC_MEASUREMENT_RUNNER").ok()?.to_string());
    // v1:<issued>:<expires>:<bearer>, with at most two 20-digit u64s.
    let mut fields = (binding.len() <= 45 + RUNNER_BEARER_LEN)
        .then_some(binding.strip_prefix("v1:")?)?
        .splitn(3, ':');
    let timestamp = |raw: &str| {
        raw.bytes()
            .all(|byte| byte.is_ascii_digit())
            .then(|| raw.parse::<u64>().ok())
            .flatten()
    };
    let issued = timestamp(fields.next()?)?;
    let expires = timestamp(fields.next()?)?;
    let token = fields.next()?;
    if !(issued..expires).contains(&now)
        || expires - issued > MAX_RUNNER_AGE_SECONDS
        || token.len() != RUNNER_BEARER_LEN
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    // Cap the normalized JS header before copying into Rust. Compare only
    // the fixed-length bearer bytes in constant time.
    let value = header(request, "authorization")?;
    if value.length() as usize != "Bearer ".len() + RUNNER_BEARER_LEN {
        return None;
    }
    let value = Zeroizing::new(String::from(value));
    let candidate = value.strip_prefix("Bearer ")?;
    bool::from(candidate.as_bytes().ct_eq(token.as_bytes())).then_some(())
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
    now: u64,
) -> Option<schedule::Case> {
    authorized_runner(request, env, now)?;
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
    let now = now_ms / 1000;
    let (lease, schedule) = admitted(request, env, now, &raw)?;
    let Some(case) = runner_case(request, env, schedule, now) else {
        return Some(crate::subsonic_staging_response::finish_document(Ok(None)));
    };
    Some(connection::respond(request, env, lease, case, now_ms).await)
}

#[cfg(feature = "staging-body-wasm-test")]
mod wasm_test {
    use super::*;
    use worker::js_sys::Array;
    use worker::wasm_bindgen::{self, prelude::*};

    #[wasm_bindgen]
    pub fn staging_measurement_case(
        request: web_sys::Request,
        env: Env,
        now: u32,
    ) -> Option<Array> {
        let raw = var(&env, "SUBSONIC_MEASUREMENT_LEASE")?;
        let now = u64::from(now);
        let (_, schedule) = admitted(&request, &env, now, &raw)?;
        let case = runner_case(&request, &env, schedule, now)?;
        case.catalogue()?;
        Some(Array::of2(
            &JsValue::from_str(case.fixture_id()),
            &case.headroom().map_or(JsValue::UNDEFINED, JsValue::from),
        ))
    }
}
