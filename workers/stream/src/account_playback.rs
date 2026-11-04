//! Grant-first, catalogue-selected playback article source. Not a public route:
//! the Worker keeps NNTP misses disabled until qualification and security review.
//! This returns encoded articles, NOT playable audio or proof of delivery. The
//! caller still owns segment verification, decryption, range slicing and actual
//! client-written metering. No cache write, prefetch or per-user pool is involved.

use std::future::Future;

use crate::account_operation::{Operation, Output};
use crate::account_pool::{with_session_deadline, WorkClass};
use crate::account_route::{ChannelKeys, RegistrySource};
use crate::account_selection::{self, AccountSource};
use crate::api_error::ApiError;
use crate::grant::{self, Grant};

/// A trusted service-side catalogue lookup, never a browser-supplied manifest,
/// queue hint, legacy demo TrackManifest or per-user provider/cache grant. Only
/// curated track mappings may select an article. Bound the lookup's response
/// while reading; do not pass user/play identity to provider services.
#[allow(async_fn_in_trait)]
pub trait PlaybackCatalogue {
    async fn article(
        &self,
        track_id: &str,
        index: usize,
    ) -> Result<Option<String>, CatalogueUnavailable>;
}

pub struct CatalogueUnavailable;

/// No account, endpoint, work class, message ID or cipher comes from a request.
pub struct ArticleRequest<'a> {
    pub token: &'a str,
    pub track_id: &'a str,
    pub index: usize,
}

/// Internal source result only. The account identity is service telemetry, not
/// user-visible metadata or authority to cache/serve these still-encoded bytes.
/// Deliberately no Debug implementation for grants or article bodies.
pub struct EncodedArticle {
    pub grant: Grant,
    pub provider_id: String,
    pub bytes: Vec<u8>,
}

fn authorize(request: &ArticleRequest<'_>, secret: &[u8], now_ms: u64) -> Result<Grant, ApiError> {
    if secret.is_empty() {
        return Err(ApiError::internal_error());
    }
    let grant = grant::verify(request.token, secret, now_ms / 1000)
        .map_err(|_| ApiError::unauthorized())?;
    // Match the public route's v2-before-track ordering. A valid v1 token for
    // another track is still unauthorized, never a track-existence signal.
    if grant.play_id.is_none() {
        return Err(ApiError::unauthorized());
    }
    if grant.track_id != request.track_id {
        return Err(ApiError::forbidden());
    }
    Ok(grant)
}

/// One catalogue-selected article under the reserved stream class, with no
/// retry/failover after operation dispatch. The <=20-second deadline includes
/// catalogue, registry, hints and BODY. Worker callers must check the hard
/// qualification gate BEFORE request/key/catalogue I/O; there is no installed
/// production catalogue adapter or runtime caller yet.
#[allow(clippy::too_many_arguments)]
pub async fn fetch(
    catalogue: &impl PlaybackCatalogue,
    registry: &impl RegistrySource,
    accounts: &impl AccountSource,
    keys: ChannelKeys<'_>,
    secret: &[u8],
    request: ArticleRequest<'_>,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
) -> Result<EncodedArticle, ApiError> {
    // Reject invalid grants even when the supplied deadline is already ready.
    authorize(&request, secret, now_ms())?;
    let work = async {
        let id = catalogue
            .article(request.track_id, request.index)
            .await
            .map_err(|_| ApiError::upstream_unavailable())?
            .ok_or_else(ApiError::upstream_unavailable)?;
        let operation = Operation::Body(id);
        if !operation.valid() {
            return Err(ApiError::upstream_unavailable());
        }
        // A slow catalogue cannot turn an expired grant into a provider read.
        let grant = authorize(&request, secret, now_ms())?;
        let served = account_selection::run(
            registry,
            accounts,
            keys,
            WorkClass::Stream,
            operation,
            &now_ms,
            std::future::pending::<()>(),
        )
        .await
        // Missing, quarantine/auth, capacity, transport and uncertain replies
        // are all generic service unavailability. Never prompt user re-auth or
        // reveal which article/account exists through the public error shape.
        .map_err(|_| ApiError::upstream_unavailable())?;
        let Output::Body(bytes) = served.output else {
            return Err(ApiError::upstream_unavailable());
        };
        Ok(EncodedArticle {
            grant,
            provider_id: served.provider_id,
            bytes,
        })
    };
    with_session_deadline(work, deadline)
        .await
        .map_err(|_| ApiError::upstream_unavailable())?
}
