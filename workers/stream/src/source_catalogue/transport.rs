//! Bounded API-binding exchange. No public fetch, route or response driver.

use std::{cell::Cell, future::Future, task::Poll};

use futures_util::future::poll_fn;
use nntune_core::{
    original_source::{
        service::{Lookup, Payload},
        valid_source_job, MAX_RECORD_BYTES,
    },
    pool_channel::MAX_REQUEST_BYTES,
};

use crate::account_playback::CatalogueUnavailable;

#[cfg(target_arch = "wasm32")]
pub mod worker;

pub(super) struct Head {
    pub status: u16,
    pub content_type: String,
    pub cache_control: String,
    pub source_job_id: String,
    pub content_length: Option<String>,
    pub content_encoding: Option<String>,
}

impl Head {
    fn validate(&self) -> Result<Option<usize>, CatalogueUnavailable> {
        if self.status != 200
            || self.content_type != "application/octet-stream"
            || !self.cache_control.eq_ignore_ascii_case("no-store")
            || !valid_source_job(&self.source_job_id)
            || self
                .content_encoding
                .as_deref()
                .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
        {
            return Err(CatalogueUnavailable);
        }
        self.content_length
            .as_deref()
            .map(|value| {
                if value.is_empty()
                    || value.len() > 10
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(CatalogueUnavailable);
                }
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0 && *n <= MAX_RECORD_BYTES)
                    .ok_or(CatalogueUnavailable)
            })
            .transpose()
    }
}

// JavaScript exposes UTF-16 length without copying the value to Wasm. Check it
// first, then bound UTF-8 bytes too; all accepted wire headers are ASCII.
fn copy_header(
    length: u32,
    limit: u32,
    copy: impl FnOnce() -> Option<String>,
) -> Result<String, CatalogueUnavailable> {
    if length > limit {
        return Err(CatalogueUnavailable);
    }
    copy()
        .filter(|value| value.len() <= limit as usize && value.is_ascii())
        .ok_or(CatalogueUnavailable)
}

/// Both the pending POST and its reply own cancellation. Dropping either must
/// abort the exchange, including a pending body read. No detached read tasks.
#[allow(async_fn_in_trait)]
pub(super) trait Client {
    type Reply: Reply;
    async fn post(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> Result<Self::Reply, CatalogueUnavailable>;
}

#[allow(async_fn_in_trait)]
pub(super) trait Reply {
    fn head(&self) -> &Head;
    /// Check the chunk length before copying it into Rust's bounded buffer.
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, CatalogueUnavailable>;
}

pub(super) async fn fetch(
    client: &impl Client,
    authorization: &str,
    body: &[u8],
    deadline_ms: u64,
    now_ms: impl Fn() -> u64,
    deadline: impl Future<Output = ()>,
) -> Result<Payload, CatalogueUnavailable> {
    let now = now_ms();
    if body.len() > MAX_REQUEST_BYTES || authorization.is_empty() || authorization.len() > 256 {
        return Err(CatalogueUnavailable);
    }
    let lookup: Lookup = serde_json::from_slice(body).map_err(|_| CatalogueUnavailable)?;
    if lookup.deadline_ms != deadline_ms || !lookup.valid_at(now) {
        return Err(CatalogueUnavailable);
    }
    let last_ms = Cell::new(now);
    let check = || {
        let now = now_ms();
        let last = last_ms.get();
        last_ms.set(last.max(now));
        if now < last || !lookup.valid_at(now) {
            Err(CatalogueUnavailable)
        } else {
            Ok(())
        }
    };
    let work = async {
        check()?;
        let mut reply = client.post(authorization, body).await?;
        check()?;
        let length = reply.head().validate()?;
        let source_job_id = reply.head().source_job_id.clone();
        let mut bytes = Vec::new();
        loop {
            check()?;
            let chunk = reply.next(MAX_RECORD_BYTES - bytes.len()).await?;
            check()?;
            let Some(chunk) = chunk else { break };
            // Defense in depth even when a Client violates its chunk contract.
            // Empty chunks fail closed rather than allowing a zero-progress loop.
            if chunk.is_empty() || chunk.len() > MAX_RECORD_BYTES - bytes.len() {
                return Err(CatalogueUnavailable);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() || length.is_some_and(|n| n != bytes.len()) {
            return Err(CatalogueUnavailable);
        }
        Ok(Payload {
            source_job_id,
            bytes,
        })
    };
    let mut work = Box::pin(work);
    let mut deadline = Box::pin(deadline);
    poll_fn(|cx| {
        if check().is_err() || deadline.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(CatalogueUnavailable));
        }
        let result = work.as_mut().poll(cx);
        // Synchronously-ready I/O can consume time without yielding to a timer.
        if check().is_err() {
            return Poll::Ready(Err(CatalogueUnavailable));
        }
        result
    })
    .await
}

#[cfg(test)]
mod tests;
