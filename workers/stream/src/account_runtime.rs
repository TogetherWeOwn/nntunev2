//! Dedicated account DO. Scheduling hints only until transport qualification.
//! Cleanup alarms and one-shot operation adapters are installed but no operation
//! RPC or product caller is enabled. Compilation is not live qualification.
//! The per-user meter DO is separate and never owns this account journal.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use nntune_core::pool_channel::{
    self, CANDIDATE_PATH, MAX_OPERATION_BYTES, MAX_REQUEST_BYTES, OPERATION_PATH,
};
use worker::{
    durable_object, Delay, DurableObject, Env, Headers, Request, Response, Result, State,
};

use zeroize::Zeroizing;

use nntune_core::transports::peer_tls::PeerTlsTarget;
use nntune_core::transports::worker_pinned_tls::{WorkerPinnedTlsTransport, WorkerSocketControl};

use crate::account_dispatch::{self, OperationReply};
use crate::account_operation::{
    AccountConnector, CleanupScheduler, DialSource, OperationError, OperationUnavailable,
};
use crate::account_pool::with_session_deadline;
use crate::account_request::{self, worker as request_io};
use crate::account_route::{
    self, CandidateCall, CandidateReply, ChannelKeys, RegistryError, RegistrySource, REGISTRY_BODY,
};
use crate::account_selection::AccountSource;
use crate::account_storage::{worker_account_owner, WorkerAccountOwner};
use crate::account_transport::{self, worker::Binding, Kind};

// Not an env toggle. Change only after actual authorized Worker transport,
// clock/entropy, alarm/physical-close and curated-track staging qualification.
const OPERATIONS_QUALIFIED: bool = false;

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
        let path = match request_io::path(&req) {
            Ok(path) => path,
            Err(_) => return respond(CandidateReply::NotFound),
        };
        let operation = path == OPERATION_PATH && OPERATIONS_QUALIFIED;
        if (path != CANDIDATE_PATH && !operation) || request_io::internal_post(&req).is_err() {
            return respond(CandidateReply::NotFound);
        }
        let cancellation = match request_io::RequestAbort::new(&req) {
            Ok(cancellation) => cancellation,
            Err(_) => return respond(CandidateReply::Unavailable),
        };
        if operation {
            return operation_response(
                account_request::run(
                    self.operation(&mut req),
                    cancellation,
                    Delay::from(Duration::from_secs(20)),
                )
                .await
                .unwrap_or(OperationReply::Failed(OperationError::Deadline)),
            );
        }
        match account_request::run(
            self.candidate(&mut req),
            cancellation,
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
    async fn operation(&self, req: &mut Request) -> OperationReply {
        let Some(owner) = &self.owner else {
            return OperationReply::NotFound;
        };
        let account_key = self.env.secret(pool_channel::ACCOUNT_SECRET_VAR).ok();
        let resolver_key = self.env.secret(pool_channel::SECRET_VAR).ok();
        let account_key = account_key.map(|s| s.to_string());
        let resolver_key = resolver_key.map(|s| s.to_string());
        let authorization = match request_io::authorization(req) {
            Ok(Some(authorization)) => authorization,
            _ => return OperationReply::NotFound,
        };
        let body = match request_io::read(req, MAX_OPERATION_BYTES).await {
            Ok(body) => body,
            Err(_) => return OperationReply::BadRequest,
        };
        account_dispatch::dispatch(
            owner,
            &WorkerRegistry(&self.env),
            &WorkerAccountConnector,
            &WorkerCleanup(&self.state),
            CandidateCall {
                method: "POST",
                path: OPERATION_PATH,
                authorization: Some(&authorization),
                body: &body,
                browser_context: false,
            },
            ChannelKeys {
                account: account_key.as_deref().map(str::as_bytes),
                resolver: resolver_key.as_deref().map(str::as_bytes),
            },
            || worker::Date::now().as_millis(),
            Delay::from(Duration::from_secs(20)),
        )
        .await
    }

    async fn candidate(&self, req: &mut Request) -> CandidateReply {
        let Some(owner) = &self.owner else {
            return CandidateReply::NotFound;
        };
        let account_key = self.env.secret(pool_channel::ACCOUNT_SECRET_VAR).ok();
        let resolver_key = self.env.secret(pool_channel::SECRET_VAR).ok();
        let account_key = account_key.map(|s| s.to_string());
        let resolver_key = resolver_key.map(|s| s.to_string());
        let authorization = match request_io::authorization(req) {
            Ok(Some(authorization)) => authorization,
            _ => return CandidateReply::NotFound,
        };
        let body = match request_io::read(req, MAX_REQUEST_BYTES).await {
            Ok(body) => body,
            Err(_) => return CandidateReply::BadRequest,
        };
        account_route::candidate(
            owner,
            &WorkerRegistry(&self.env),
            CandidateCall {
                method: "POST",
                path: CANDIDATE_PATH,
                authorization: Some(&authorization),
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

/// The old per-user DO is only an ingress router for these new paths. It
/// cannot provide an account ID/class, resolve a user vault or own a socket.
/// Keep the qualification check before request body and internal key reads.
pub(crate) async fn ingest_fetch(
    env: &Env,
    pool_user: Option<&str>,
    req: &mut Request,
) -> Result<Response> {
    if !OPERATIONS_QUALIFIED || pool_user.is_none() || request_io::internal_post(req).is_err() {
        return operation_response(OperationReply::NotFound);
    }
    let path = match request_io::path(req) {
        Ok(path) => path,
        Err(_) => return operation_response(OperationReply::NotFound),
    };
    let cancellation = match request_io::RequestAbort::new(req) {
        Ok(cancellation) => cancellation,
        Err(_) => return operation_response(OperationReply::Failed(OperationError::Unavailable)),
    };
    let work = async {
        let cap = match path.as_str() {
            nntune_core::pool_operation::INGEST_BODY_PATH => crate::body::MAX_BODY_REQUEST_BYTES,
            nntune_core::pool_operation::INGEST_STAT_PATH => crate::probe::MAX_PROBE_BODY_BYTES,
            _ => return OperationReply::NotFound,
        };
        let body = match request_io::read(req, cap).await {
            Ok(body) => body,
            Err(_) => return OperationReply::BadRequest,
        };
        let account_key = env
            .secret(pool_channel::ACCOUNT_SECRET_VAR)
            .ok()
            .map(|s| s.to_string());
        let resolver_key = env
            .secret(pool_channel::SECRET_VAR)
            .ok()
            .map(|s| s.to_string());
        crate::account_ingress::run(
            &WorkerRegistry(env),
            &WorkerAccounts(env),
            ChannelKeys {
                account: account_key.as_deref().map(str::as_bytes),
                resolver: resolver_key.as_deref().map(str::as_bytes),
            },
            &path,
            &body,
            pool_user.unwrap_or(""),
            || worker::Date::now().as_millis(),
            Delay::from(Duration::from_secs(20)),
        )
        .await
    };
    operation_response(
        account_request::run(work, cancellation, Delay::from(Duration::from_secs(20)))
            .await
            .unwrap_or(OperationReply::Failed(OperationError::Deadline)),
    )
}

struct WorkerRegistry<'a>(&'a Env);

impl RegistrySource for WorkerRegistry<'_> {
    async fn fetch(&self, authorization: &str) -> std::result::Result<Vec<u8>, RegistryError> {
        with_session_deadline(
            self.read(authorization, Kind::Registry, REGISTRY_BODY),
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
            self.read(authorization, Kind::Resolve, body),
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
        kind: Kind,
        body: &[u8],
    ) -> std::result::Result<Zeroizing<Vec<u8>>, RegistryError> {
        let response = account_transport::exchange(
            &Binding::Api { env: self.0, kind },
            kind,
            authorization,
            body,
        )
        .await?;
        if response.head.status != 200 {
            return Err(RegistryError);
        }
        Ok(response.bytes)
    }
}

/// Qualification/client adapter; no product caller invokes it. The server's
/// operation gate remains false even if internal credentials are configured.
pub struct WorkerAccounts<'a>(pub &'a Env);

impl AccountSource for WorkerAccounts<'_> {
    async fn candidate(
        &self,
        account_id: &str,
        authorization: &str,
        body: &[u8],
    ) -> std::result::Result<Vec<u8>, RegistryError> {
        let mut reply = with_session_deadline(
            self.call(account_id, Kind::Candidate, authorization, body),
            Delay::from(Duration::from_secs(4)),
        )
        .await
        .map_err(|_| RegistryError)??;
        if reply.head.status != 200 {
            return Err(RegistryError);
        }
        Ok(std::mem::take(&mut *reply.bytes))
    }

    async fn execute(&self, account_id: &str, authorization: &str, body: &[u8]) -> OperationReply {
        match with_session_deadline(
            self.call(account_id, Kind::Operation, authorization, body),
            Delay::from(Duration::from_secs(20)),
        )
        .await
        {
            Ok(Ok(mut response)) => account_dispatch::decode_response(
                response.head.status,
                &response.head.content_type,
                response.head.provider_id.as_deref(),
                std::mem::take(&mut *response.bytes),
            ),
            Ok(Err(_)) => OperationReply::Failed(OperationError::Unavailable),
            Err(_) => OperationReply::Failed(OperationError::Deadline),
        }
    }
}

impl WorkerAccounts<'_> {
    async fn call(
        &self,
        account_id: &str,
        kind: Kind,
        authorization: &str,
        body: &[u8],
    ) -> std::result::Result<account_transport::Response, RegistryError> {
        account_transport::exchange(
            &Binding::Account {
                env: self.0,
                account_id,
                kind,
            },
            kind,
            authorization,
            body,
        )
        .await
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

/// Qualification adapter only. No operation RPC or product caller is enabled.
/// The gated operation handler composes retained owner, WorkerRegistry,
/// WorkerCleanup and a <=20-second whole-call Delay; never call this directly
/// as an unfenced legacy dial closure.
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

fn operation_response(reply: OperationReply) -> Result<Response> {
    let reply = reply.into_response();
    let headers = Headers::new();
    headers.set("content-type", reply.content_type)?;
    headers.set("cache-control", "no-store")?;
    if let Some(provider_id) = reply.provider_id {
        headers.set(account_dispatch::PROVIDER_HEADER, &provider_id)?;
    }
    Ok(Response::from_bytes(reply.body)?
        .with_status(reply.status)
        .with_headers(headers))
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
