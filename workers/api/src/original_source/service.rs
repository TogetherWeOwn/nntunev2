//! Off-route authenticated source-service composition. No Worker fetch route,
//! database connection, key provisioning or public caller is installed here.

use std::{cell::Cell, future::Future, task::Poll, time::Duration};

use futures_util::future::poll_fn;
use nntune_core::{
    original_source::service::{Lookup, Payload, PATH},
    pool_channel::Channel,
};

use super::{
    read_original_source_async, AsyncSourceRepository, SourceReadAuthorization, SourceServiceError,
};
use crate::rotation::KekSource;

pub mod http;
#[cfg(target_arch = "wasm32")]
pub mod worker;

/// Construction is inert. connect must be cancellation-safe and must not keep a
/// query/connection task alive after its future or returned repository is dropped.
#[allow(async_fn_in_trait)]
pub trait RepositoryFactory {
    type Repository: AsyncSourceRepository;
    async fn connect(&self) -> Result<Self::Repository, SourceServiceError>;
}

pub struct Call<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub authorization: Option<&'a str>,
    pub body: &'a [u8],
    pub browser_context: bool,
}

struct Authority<F> {
    lookup: Lookup,
    now_ms: F,
    last_ms: Cell<u64>,
}

impl<F: Fn() -> u64> SourceReadAuthorization for Authority<F> {
    fn authorize(&self, track_id: &str) -> Result<(), SourceServiceError> {
        let now = (self.now_ms)();
        let last = self.last_ms.get();
        self.last_ms.set(last.max(now));
        if now < last || track_id != self.lookup.track_id || !self.lookup.valid_at(now) {
            return Err(SourceServiceError::Denied);
        }
        Ok(())
    }
}

/// Authenticate before connection creation; the signed attestation is issued by
/// the grant-verifying stream caller, not decoded from a public raw-source body.
/// Missing/invalid channel, browser context and malformed authority all deny
/// before I/O. No retry, fallback key, cache or account lookup is permitted.
///
/// Runtime must supply a deadline that fires no later than the signed absolute
/// deadline (at most five seconds), covering connect, queries and serialization.
/// Cancellation drops the entire operation. A later Worker adapter must return
/// only no-store service-binding responses and collapse failures publicly.
pub async fn read(
    factory: &impl RepositoryFactory,
    keks: &impl KekSource,
    play_grant_key: Option<&[u8]>,
    call: Call<'_>,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
) -> Result<Payload, SourceServiceError> {
    let now = now_ms();
    let authority = Authority {
        lookup: authenticate(play_grant_key, call, now)?,
        now_ms,
        last_ms: Cell::new(now),
    };
    read_authorized(factory, keks, authority, deadline).await
}

/// Runtime entry for the off-route repository factory. One timer covers the
/// signed time remaining, not a fresh five-second budget at every await. It is
/// polled before connect and dropped with the request; no timer task is spawned.
pub async fn read_timed(
    factory: &impl RepositoryFactory,
    keks: &impl KekSource,
    play_grant_key: Option<&[u8]>,
    call: Call<'_>,
    now_ms: impl Fn() -> u64,
) -> Result<Payload, SourceServiceError> {
    let now = now_ms();
    let lookup = authenticate(play_grant_key, call, now)?;
    let remaining = Duration::from_millis(lookup.deadline_ms - now);
    #[cfg(target_arch = "wasm32")]
    let deadline = ::worker::Delay::from(remaining);
    #[cfg(not(target_arch = "wasm32"))]
    let deadline = futures_timer::Delay::new(remaining);
    read_authorized(
        factory,
        keks,
        Authority {
            lookup,
            now_ms,
            last_ms: Cell::new(now),
        },
        deadline,
    )
    .await
}

fn authenticate(
    play_grant_key: Option<&[u8]>,
    call: Call<'_>,
    now: u64,
) -> Result<Lookup, SourceServiceError> {
    if call.method != "POST"
        || call.path != PATH
        || call.browser_context
        || Channel::OriginalSource
            .verify(
                play_grant_key,
                call.path,
                call.authorization,
                call.body,
                now / 1000,
            )
            .is_err()
        || call.body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{')
    {
        return Err(SourceServiceError::Denied);
    }
    let lookup: Lookup =
        serde_json::from_slice(call.body).map_err(|_| SourceServiceError::Denied)?;
    if !lookup.valid_at(now) {
        return Err(SourceServiceError::Denied);
    }
    Ok(lookup)
}

async fn read_authorized(
    factory: &impl RepositoryFactory,
    keks: &impl KekSource,
    authority: Authority<impl Fn() -> u64>,
    deadline: impl Future<Output = ()>,
) -> Result<Payload, SourceServiceError> {
    let track_id = &authority.lookup.track_id;
    authority.authorize(track_id)?;
    let work = async {
        // Recheck on the first poll too, not merely when this future was created.
        authority.authorize(track_id)?;
        let mut repository = factory.connect().await?;
        authority.authorize(track_id)?;
        let source =
            read_original_source_async(&authority, &mut repository, keks, track_id).await?;
        let payload = Payload {
            source_job_id: source.source_job_id().into(),
            bytes: source.encode().map_err(|_| SourceServiceError::Invalid)?,
        };
        authority.authorize(track_id)?;
        Ok(payload)
    };
    let mut work = Box::pin(work);
    let mut deadline = Box::pin(deadline);
    poll_fn(|cx| {
        if deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(SourceServiceError::Denied));
        }
        work.as_mut().poll(cx)
    })
    .await
}

#[cfg(test)]
mod tests;
