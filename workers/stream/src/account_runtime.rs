//! Dedicated account DO. Scheduling hints only until transport qualification.
//! The per-user meter DO is separate and never owns this account journal.

use std::time::Duration;

use futures_util::StreamExt;
use nntune_core::pool_channel::{self, CANDIDATE_PATH, MAX_REQUEST_BYTES};
use worker::wasm_bindgen::JsValue;
use worker::{
    durable_object, Delay, DurableObject, Env, Headers, Method, Request, RequestInit,
    RequestRedirect, Response, Result, State,
};

use crate::account_pool::with_session_deadline;
use crate::account_route::{
    self, CandidateCall, CandidateReply, ChannelKeys, RegistryError, RegistrySource,
    MAX_REGISTRY_BYTES, REGISTRY_BODY,
};
use crate::account_storage::{worker_account_owner, WorkerAccountOwner};

#[durable_object]
pub struct NntpAccount {
    env: Env,
    // Construct exactly once. Missing/non-account object names stay fail-closed.
    owner: Option<WorkerAccountOwner>,
}

impl DurableObject for NntpAccount {
    fn new(state: State, env: Env) -> Self {
        Self {
            owner: worker_account_owner(&state).ok(),
            env,
        }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        if req.method() != Method::Post
            || req.path() != CANDIDATE_PATH
            || req.url()?.query().is_some()
            || req.headers().has("origin")?
            || req.headers().has("cookie")?
        {
            return respond(CandidateReply::NotFound);
        }
        match with_session_deadline(
            self.candidate(&mut req),
            Delay::from(Duration::from_secs(10)),
        )
        .await
        {
            Ok(reply) => respond(reply),
            Err(_) => respond(CandidateReply::Unavailable),
        }
    }
}

impl NntpAccount {
    async fn candidate(&self, req: &mut Request) -> CandidateReply {
        let Some(owner) = &self.owner else {
            return CandidateReply::NotFound;
        };
        let account_key = self.env.secret(pool_channel::ACCOUNT_SECRET_VAR).ok();
        let resolver_key = self.env.secret(pool_channel::SECRET_VAR).ok();
        let account_key = account_key.map(|s| s.to_string());
        let resolver_key = resolver_key.map(|s| s.to_string());
        let authorization = req.headers().get("authorization").ok().flatten();
        let body = match read_request(req).await {
            Ok(body) => body,
            Err(_) => return CandidateReply::BadRequest,
        };
        account_route::candidate(
            owner,
            &WorkerRegistry(&self.env),
            CandidateCall {
                method: "POST",
                path: CANDIDATE_PATH,
                authorization: authorization.as_deref(),
                body: &body,
                browser_context: false,
            },
            ChannelKeys {
                account: account_key.as_deref().map(str::as_bytes),
                resolver: resolver_key.as_deref().map(str::as_bytes),
            },
            || worker::Date::now().as_millis(),
        )
        .await
    }
}

struct WorkerRegistry<'a>(&'a Env);

impl RegistrySource for WorkerRegistry<'_> {
    async fn fetch(&self, authorization: &str) -> std::result::Result<Vec<u8>, RegistryError> {
        with_session_deadline(
            self.read(authorization),
            Delay::from(Duration::from_secs(3)),
        )
        .await
        .map_err(|_| RegistryError)?
    }
}

impl WorkerRegistry<'_> {
    async fn read(&self, authorization: &str) -> std::result::Result<Vec<u8>, RegistryError> {
        let api = self.0.service("API").map_err(|_| RegistryError)?;
        let headers = Headers::new();
        headers
            .set("authorization", authorization)
            .map_err(|_| RegistryError)?;
        headers
            .set("content-type", "application/json")
            .map_err(|_| RegistryError)?;
        headers
            .set("cache-control", "no-store")
            .map_err(|_| RegistryError)?;
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error)
            .with_body(Some(JsValue::from_str(
                std::str::from_utf8(REGISTRY_BODY).map_err(|_| RegistryError)?,
            )));
        // This URL is routed only by the service binding, never public fetch.
        let url = format!("https://nntune-api{}", pool_channel::REGISTRY_PATH);
        let request = Request::new_with_init(&url, &init).map_err(|_| RegistryError)?;
        let mut response = api
            .fetch_request(request)
            .await
            .map_err(|_| RegistryError)?;
        if response.status_code() != 200 {
            return Err(RegistryError);
        }
        let mut stream = response.stream().map_err(|_| RegistryError)?;
        let mut raw = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| RegistryError)?;
            if raw.len().saturating_add(chunk.len()) > MAX_REGISTRY_BYTES {
                return Err(RegistryError);
            }
            raw.extend_from_slice(&chunk);
        }
        Ok(raw)
    }
}

async fn read_request(req: &mut Request) -> std::result::Result<Vec<u8>, RegistryError> {
    let mut stream = req.stream().map_err(|_| RegistryError)?;
    let mut raw = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| RegistryError)?;
        if raw.len().saturating_add(chunk.len()) > MAX_REQUEST_BYTES {
            return Err(RegistryError);
        }
        raw.extend_from_slice(&chunk);
    }
    Ok(raw)
}

fn respond(reply: CandidateReply) -> Result<Response> {
    let (status, body) = match reply {
        CandidateReply::NotFound => (404, r#"{"error":{"code":"not_found"}}"#.into()),
        CandidateReply::BadRequest => (400, r#"{"error":{"code":"invalid_request"}}"#.into()),
        CandidateReply::Unavailable => (503, r#"{"error":{"code":"provider_unavailable"}}"#.into()),
        CandidateReply::Hint(candidate) => (200, serde_json::to_string(&candidate)?),
    };
    let headers = Headers::new();
    headers.set("content-type", "application/json")?;
    headers.set("cache-control", "no-store")?;
    Ok(Response::ok(body)?
        .with_status(status)
        .with_headers(headers))
}
