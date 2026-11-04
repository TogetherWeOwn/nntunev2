//! Bounded internal HTTP composition, deliberately not registered with fetch.
//! The caller supplies wakeable cancellation and a whole-request deadline. No
//! session, public catalogue, provider, cache or delivered-byte authority lives here.

use std::{cell::Cell, future::Future, task::Poll};

use futures_util::future::poll_fn;
use nntune_core::{
    original_source::{service::PATH, valid_source_job, MAX_RECORD_BYTES},
    pool_channel::MAX_REQUEST_BYTES,
};

use super::{Call, RepositoryFactory};
use crate::{original_source::SourceServiceError, rotation::KekSource};

pub struct Head<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub authorization: Option<&'a str>,
    pub content_type: Option<&'a str>,
    pub content_length: Option<&'a str>,
    pub content_encoding: Option<&'a str>,
    pub browser_context: bool,
}

/// Implementations must bound each raw chunk before copying it into Rust and
/// stop underlying reads when dropped. No detached body-reader task is allowed.
#[allow(async_fn_in_trait)]
pub trait Body {
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, SourceServiceError>;
}

pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub cache_control: &'static str,
    pub source_job_id: Option<String>,
    pub bytes: Vec<u8>,
}

impl Reply {
    fn unavailable() -> Self {
        Self {
            status: 503,
            content_type: "application/octet-stream",
            cache_control: "no-store",
            source_job_id: None,
            bytes: Vec::new(),
        }
    }
}

impl Head<'_> {
    fn validate(&self) -> Result<Option<usize>, SourceServiceError> {
        if self.method != "POST"
            || self.path != PATH
            || self.browser_context
            || self.content_type != Some("application/json")
            || self.content_encoding.is_some()
            || !self
                .authorization
                .is_some_and(|auth| !auth.is_empty() && auth.len() <= 256)
        {
            return Err(SourceServiceError::Denied);
        }
        self.content_length
            .map(|value| {
                if value.is_empty()
                    || value.len() > 10
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(SourceServiceError::Denied);
                }
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|&len| len > 0 && len <= MAX_REQUEST_BYTES)
                    .ok_or(SourceServiceError::Denied)
            })
            .transpose()
    }
}

/// Both controls are polled before body collection/database work, including on
/// the first poll. The deadline covers body collection too; service::read_timed
/// additionally enforces the signed remaining lifetime after authentication.
/// Every failure has one empty no-store response with no source-job disclosure.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    factory: &impl RepositoryFactory,
    keks: &impl KekSource,
    play_grant_key: Option<&[u8]>,
    head: Head<'_>,
    mut body: impl Body,
    now_ms: impl Fn() -> u64,
    cancellation: impl Future<Output = ()>,
    deadline: impl Future<Output = ()>,
) -> Reply {
    let initial = now_ms();
    let last = Cell::new(initial);
    let clock_ok = Cell::new(true);
    let clock = || {
        let now = now_ms();
        let previous = last.replace(last.get().max(now));
        if now < previous {
            clock_ok.set(false);
        }
        if clock_ok.get() {
            now
        } else {
            u64::MAX
        }
    };
    let work = async {
        let declared = head.validate()?;
        let mut bytes = Vec::new();
        loop {
            clock();
            if !clock_ok.get() {
                return Err(SourceServiceError::Denied);
            }
            let Some(chunk) = body.next(MAX_REQUEST_BYTES - bytes.len()).await? else {
                break;
            };
            if chunk.is_empty() || chunk.len() > MAX_REQUEST_BYTES - bytes.len() {
                return Err(SourceServiceError::Denied);
            }
            bytes.extend_from_slice(&chunk);
        }
        clock();
        if !clock_ok.get() || bytes.is_empty() || declared.is_some_and(|len| len != bytes.len()) {
            return Err(SourceServiceError::Denied);
        }
        let payload = super::read_timed(
            factory,
            keks,
            play_grant_key,
            Call {
                method: head.method,
                path: head.path,
                authorization: head.authorization,
                body: &bytes,
                browser_context: head.browser_context,
            },
            &clock,
        )
        .await?;
        clock();
        if !clock_ok.get()
            || payload.bytes.is_empty()
            || payload.bytes.len() > MAX_RECORD_BYTES
            || !valid_source_job(&payload.source_job_id)
        {
            return Err(SourceServiceError::Invalid);
        }
        Ok(Reply {
            status: 200,
            content_type: "application/octet-stream",
            cache_control: "no-store",
            source_job_id: Some(payload.source_job_id),
            bytes: payload.bytes,
        })
    };
    let mut work = Box::pin(work);
    let mut cancellation = Box::pin(cancellation);
    let mut deadline = Box::pin(deadline);
    poll_fn(|cx| {
        if cancellation.as_mut().poll(cx).is_ready() || deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Reply::unavailable());
        }
        match work.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                // A control may have become ready while work was polled.
                if cancellation.as_mut().poll(cx).is_ready()
                    || deadline.as_mut().poll(cx).is_ready()
                {
                    return Poll::Ready(Reply::unavailable());
                }
                Poll::Ready(result.unwrap_or_else(|_| Reply::unavailable()))
            }
        }
    })
    .await
}
