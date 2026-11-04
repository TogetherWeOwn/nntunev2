//! Grant-first, catalogue-selected playback article source. Not a public route:
//! the Worker keeps NNTP misses disabled until qualification and security review.
//! This returns encoded articles, NOT playable audio or proof of delivery. The
//! caller still owns segment verification, decryption, range slicing and actual
//! client-written metering. No cache write, prefetch or per-user pool is involved.

pub mod control;

use std::{future::Future, io::Cursor, ops::Range};

use nntune_core::original_source::{
    reader::{RangeReader, ReadControl},
    OriginalSource, SourceError,
};

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

/// The existing API/keyring authority opens a bounded, track-bound source.
/// This seam must not deserialize a public manifest or export envelope keys.
/// No production service-binding/catalogue adapter is installed yet.
#[allow(async_fn_in_trait)]
pub trait OriginalCatalogue {
    async fn source(&self, track_id: &str) -> Result<Option<OriginalSource>, CatalogueUnavailable>;
}

pub struct OriginalRangeRequest<'a> {
    pub token: &'a str,
    pub track_id: &'a str,
    /// Validated half-open original-file coordinates, never encoded offsets.
    pub range: Range<u64>,
}

/// Internal verification receipt, not delivered-byte accounting or cache
/// authority. Accounts include every fetched article, including final proof;
/// their identities never cross a public response boundary. No Debug.
pub struct OriginalRangeReceipt {
    pub grant: Grant,
    pub verified_bytes: u64,
    pub accounts: Vec<String>,
}

struct GrantControl<'a, F> {
    request: ArticleRequest<'a>,
    secret: &'a [u8],
    now_ms: &'a F,
}

impl<F: Fn() -> u64> ReadControl for GrantControl<'_, F> {
    fn check(&mut self) -> Result<(), SourceError> {
        authorize(&self.request, self.secret, (self.now_ms)())
            .map(|_| ())
            .map_err(|_| SourceError::Cancelled)
    }
}

/// Grant-first async composition of the approved original-source reader and
/// fixed Stream BODY dispatch. The same <=20-second caller deadline covers
/// lookup, all article operations and the verified prefix walk. At most one
/// dispatch per requested article; any uncertain/failing operation aborts with
/// no retry/failover. Never collects a file-sized output Vec. A sink failure or
/// any later error requires the caller to abort its partial response, without
/// recording full-file success. This is not the posted-file decrypt path.
///
/// Worker callers must check the hard qualification gate before keys/lookup/
/// request I/O. There is intentionally no runtime caller or production adapter.
#[allow(clippy::too_many_arguments)]
pub async fn read_original_range(
    catalogue: &impl OriginalCatalogue,
    registry: &impl RegistrySource,
    accounts: &impl AccountSource,
    keys: ChannelKeys<'_>,
    secret: &[u8],
    request: OriginalRangeRequest<'_>,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
    mut sink: impl FnMut(&[u8]) -> Result<(), SourceError>,
) -> Result<OriginalRangeReceipt, ApiError> {
    let article_request = ArticleRequest {
        token: request.token,
        track_id: request.track_id,
        index: 0,
    };
    authorize(&article_request, secret, now_ms())?;
    let work = async {
        let source = catalogue
            .source(request.track_id)
            .await
            .map_err(|_| ApiError::upstream_unavailable())?
            .ok_or_else(ApiError::upstream_unavailable)?;
        if source.track_id() != request.track_id {
            return Err(ApiError::upstream_unavailable());
        }
        // Revalidate after lookup, before range/existence or provider work.
        authorize(&article_request, secret, now_ms())?;
        let mut reader = RangeReader::new(&source, request.range.clone())
            .map_err(|_| ApiError::range_not_satisfiable())?;
        let mut control = GrantControl {
            request: article_request,
            secret,
            now_ms: &now_ms,
        };
        let mut serving_accounts = Vec::new();
        let mut verified_bytes = 0u64;
        while let Some(reference) = reader
            .next_reference()
            .map_err(|_| ApiError::upstream_unavailable())?
        {
            authorize(&control.request, secret, now_ms())?;
            let operation = Operation::Body(reference.to_owned());
            if !operation.valid() {
                return Err(ApiError::upstream_unavailable());
            }
            let served = account_selection::run(
                registry,
                accounts,
                ChannelKeys {
                    account: keys.account,
                    resolver: keys.resolver,
                },
                WorkClass::Stream,
                operation,
                &now_ms,
                std::future::pending::<()>(),
            )
            .await
            .map_err(|_| ApiError::upstream_unavailable())?;
            authorize(&control.request, secret, now_ms())?;
            let Output::Body(bytes) = served.output else {
                return Err(ApiError::upstream_unavailable());
            };
            serving_accounts.push(served.provider_id);
            reader
                .push(Cursor::new(bytes), &mut control, |slice| {
                    sink(slice)?;
                    verified_bytes = verified_bytes
                        .checked_add(slice.len() as u64)
                        .ok_or(SourceError::Bounds)?;
                    Ok(())
                })
                .map_err(|_| ApiError::upstream_unavailable())?;
        }
        let grant = authorize(&control.request, secret, now_ms())?;
        if verified_bytes != request.range.end - request.range.start {
            return Err(ApiError::upstream_unavailable());
        }
        Ok(OriginalRangeReceipt {
            grant,
            verified_bytes,
            accounts: serving_accounts,
        })
    };
    with_session_deadline(work, deadline)
        .await
        .map_err(|_| ApiError::upstream_unavailable())?
}
