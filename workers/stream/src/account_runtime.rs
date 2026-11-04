//! Dedicated account DO. Scheduling hints only until transport qualification.
//! Cleanup alarms and one-shot operation adapters are installed but no operation
//! RPC or product caller is enabled. Compilation is not live qualification.
//! The per-user meter DO is separate and never owns this account journal.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use nntune_core::pool_channel::{self, CANDIDATE_PATH, MAX_REQUEST_BYTES};
use worker::wasm_bindgen::JsValue;
use worker::{
    durable_object, Delay, DurableObject, Env, Headers, Method, Request, RequestInit,
    RequestRedirect, Response, Result, State,
};

use zeroize::Zeroizing;

use nntune_core::transports::peer_tls::PeerTlsTarget;
use nntune_core::transports::worker_pinned_tls::{WorkerPinnedTlsTransport, WorkerSocketControl};

use crate::account_operation::{
    AccountConnector, CleanupScheduler, DialSource, OperationUnavailable, MAX_DIAL_BYTES,
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
    state: State,
    // Construct exactly once. Missing/non-account object names stay fail-closed.
    owner: Option<WorkerAccountOwner>,
}

impl DurableObject for NntpAccount {
    fn new(state: State, env: Env) -> Self {
        Self {
            owner: worker_account_owner(&state).ok(),
            env,
            state,
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

    async fn alarm(&self) -> Result<Response> {
        let Some(owner) = &self.owner else {
            return Response::empty();
        };
        if !owner.needs_cleanup().await {
            return Response::empty();
        }
        // A failed/slow registry read must never suppress retained-control
        // cleanup. The owner keeps its high-water clock through both paths.
        let refresh = async {
            let key = self
                .env
                .secret(pool_channel::SECRET_VAR)
                .ok()
                .map(|s| s.to_string());
            let _ = account_route::refresh(
                owner,
                &WorkerRegistry(&self.env),
                key.as_deref().map(str::as_bytes),
                || worker::Date::now().as_millis(),
            )
            .await;
        };
        let _ = with_session_deadline(refresh, Delay::from(Duration::from_secs(4))).await;
        let _ = with_session_deadline(
            owner.cleanup(worker::Date::now().as_millis()),
            Delay::from(Duration::from_secs(3)),
        )
        .await;
        // No alarm deletion: a concurrent admission's earlier wake must survive.
        // Recovered/poisoned bookkeeping alone does not spin forever; only
        // unclosed controls or cancellable unstarted reservations need a wake.
        if owner.needs_cleanup().await {
            WorkerCleanup(&self.state)
                .ensure_cleanup()
                .await
                .map_err(|_| {
                    worker::Error::RustError("account cleanup could not schedule its alarm".into())
                })?;
        }
        Response::empty()
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
            self.read(
                authorization,
                pool_channel::REGISTRY_PATH,
                REGISTRY_BODY,
                MAX_REGISTRY_BYTES,
            ),
            Delay::from(Duration::from_secs(3)),
        )
        .await
        .map_err(|_| RegistryError)?
        .map(|mut raw| std::mem::take(&mut *raw))
    }
}

impl DialSource for WorkerRegistry<'_> {
    async fn resolve(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> std::result::Result<Zeroizing<Vec<u8>>, OperationUnavailable> {
        with_session_deadline(
            self.read(
                authorization,
                pool_channel::RESOLVE_PATH,
                body,
                MAX_DIAL_BYTES,
            ),
            Delay::from(Duration::from_secs(3)),
        )
        .await
        .map_err(|_| OperationUnavailable)?
        .map_err(|_| OperationUnavailable)
    }
}

impl WorkerRegistry<'_> {
    async fn read(
        &self,
        authorization: &str,
        path: &str,
        body: &[u8],
        max_bytes: usize,
    ) -> std::result::Result<Zeroizing<Vec<u8>>, RegistryError> {
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
                std::str::from_utf8(body).map_err(|_| RegistryError)?,
            )));
        // This URL is routed only by the service binding, never public fetch.
        let url = format!("https://nntune-api{path}");
        let request = Request::new_with_init(&url, &init).map_err(|_| RegistryError)?;
        let mut response = api
            .fetch_request(request)
            .await
            .map_err(|_| RegistryError)?;
        if response.status_code() != 200 {
            return Err(RegistryError);
        }
        let mut stream = response.stream().map_err(|_| RegistryError)?;
        let mut raw = Zeroizing::new(Vec::new());
        while let Some(chunk) = stream.next().await {
            let chunk = Zeroizing::new(chunk.map_err(|_| RegistryError)?);
            if raw.len().saturating_add(chunk.len()) > max_bytes {
                return Err(RegistryError);
            }
            raw.extend_from_slice(&chunk);
        }
        Ok(raw)
    }
}

struct WorkerCleanup<'a>(&'a State);

impl CleanupScheduler for WorkerCleanup<'_> {
    async fn ensure_cleanup(&self) -> std::result::Result<(), OperationUnavailable> {
        with_session_deadline(
            async {
                let storage = self.0.storage();
                let now = i64::try_from(worker::Date::now().as_millis())
                    .map_err(|_| OperationUnavailable)?;
                match storage
                    .get_alarm()
                    .await
                    .map_err(|_| OperationUnavailable)?
                {
                    Some(at) if at > now && at <= now.saturating_add(1000) => {}
                    _ => storage
                        .set_alarm(1000_i64)
                        .await
                        .map_err(|_| OperationUnavailable)?,
                }
                storage.sync().await.map_err(|_| OperationUnavailable)
            },
            Delay::from(Duration::from_secs(2)),
        )
        .await
        .map_err(|_| OperationUnavailable)?
    }
}

/// Qualification adapter only. There is deliberately no operation RPC/caller.
/// The eventual operation route must use the retained owner, WorkerRegistry,
/// WorkerCleanup and a <=20-second whole-session Delay together; never call this
/// directly as an unfenced legacy dial closure.
pub struct WorkerAccountConnector;

impl AccountConnector for WorkerAccountConnector {
    type Control = WorkerSocketControl;
    type Connection = WorkerPinnedTlsTransport;

    fn open(
        &self,
        peer: SocketAddr,
        tls_name: String,
    ) -> io::Result<(
        Self::Control,
        impl Future<Output = io::Result<Self::Connection>>,
    )> {
        WorkerPinnedTlsTransport::open(PeerTlsTarget::new(peer, tls_name)?, Duration::from_secs(3))
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
