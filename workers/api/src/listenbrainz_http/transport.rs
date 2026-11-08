//! Private bounded Worker fetch primitive. The coordinator owns durable
//! fleet-wide claim/send/finish ordering, including body/transport failures.
//! Deliberately not wired to a user route.

use std::{cell::Cell, future::Future};

use futures_util::{Stream, StreamExt};
use worker::{Fetch, Headers, Method, Request, RequestInit, RequestRedirect};
use zeroize::Zeroizing;

use super::{ApiRequest, WireError, MAX_VALIDATION_BODY_BYTES, TOTAL_TIMEOUT_MS};
use crate::{listenbrainz, worker_entry::with_deadline};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ResponseHead {
    pub status: u16,
    pub cooldown_s: u64,
}

impl ResponseHead {
    fn new(now_s: u64, status: u16, headers: &[(&str, &str)]) -> Self {
        // Fetch Headers may coalesce duplicate fields. The documented reset
        // and remaining fields are numeric, so restore their separate values.
        // Do NOT split Retry-After: an HTTP-date contains a comma and remains
        // an unsupported value that the parser deliberately denies forever.
        let mut expanded = Vec::new();
        for (name, value) in headers {
            if name.eq_ignore_ascii_case("retry-after") {
                expanded.push((*name, *value));
            } else {
                expanded.extend(value.split(',').map(|value| (*name, value)));
            }
        }
        let at = listenbrainz::next_allowed_at(now_s, status, &expanded);
        Self {
            status,
            // Preserve the parser's fail-closed sentinel rather than turning
            // it into a slightly shorter relative wait near u64::MAX.
            cooldown_s: if at == u64::MAX {
                u64::MAX
            } else {
                at.saturating_sub(now_s)
            },
        }
    }
}

/// No Debug/Serialize: even an upstream error may echo a submitted token.
/// There is intentionally no Result at the outer seam that could discard a
/// received rate-limit header on a later stream failure or deadline.
#[must_use = "persist the cooldown before interpreting any upstream outcome"]
pub(crate) struct Exchange {
    pub head: Option<ResponseHead>,
    pub body: Result<Zeroizing<Vec<u8>>, WireError>,
}

impl Exchange {
    pub fn cooldown_s(&self) -> u64 {
        self.head
            .map(|head| head.cooldown_s)
            .unwrap_or(listenbrainz::DEFAULT_429_WAIT_S)
    }
}

struct AbortOnDrop(Option<worker::AbortController>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(controller) = self.0.take() {
            controller.abort();
        }
    }
}

/// Keep the head outside the timed future so a deadline during body collection
/// cannot erase a provider cooldown. The single timer covers headers AND body;
/// expiry drops the fetch future (whose guard aborts the actual JS fetch).
async fn timed_exchange(
    head: &Cell<Option<ResponseHead>>,
    operation: impl Future<Output = Result<Zeroizing<Vec<u8>>, WireError>>,
    timeout_ms: u64,
) -> Exchange {
    let body = with_deadline(operation, timeout_ms, WireError::Unavailable).await;
    Exchange {
        head: head.get(),
        body,
    }
}

/// Cap BEFORE copying a chunk into the application buffer. Workers-rs itself
/// allocates each runtime chunk; this bound is on retained Rust body bytes,
/// not on a chunk already allocated by the runtime. Partial/error bodies are
/// zeroized, never parsed. Declared length is only an early rejection hint:
/// compressed responses may have a different decoded length.
async fn validation_body<S, E>(
    declared_len: Option<usize>,
    stream: S,
) -> Result<Zeroizing<Vec<u8>>, WireError>
where
    S: Stream<Item = Result<Vec<u8>, E>>,
{
    if declared_len.is_some_and(|len| len > MAX_VALIDATION_BODY_BYTES) {
        return Err(WireError::InvalidResponse);
    }
    let mut stream = std::pin::pin!(stream);
    // Fixed capacity also bounds Vec's allocation growth, not just its len.
    let mut body = Zeroizing::new(Vec::with_capacity(MAX_VALIDATION_BODY_BYTES));
    let mut chunks = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = Zeroizing::new(chunk.map_err(|_| WireError::Unavailable)?);
        chunks += 1;
        // Empty ready chunks cannot starve the enclosing deadline forever.
        if chunks > MAX_VALIDATION_BODY_BYTES + 1 {
            return Err(WireError::InvalidResponse);
        }
        if chunk.len() > MAX_VALIDATION_BODY_BYTES - body.len() {
            return Err(WireError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn redirect_policy() -> RequestRedirect {
    RequestRedirect::Manual
}

/// Only fixed `ApiRequest` endpoints are representable. Redirects are returned
/// as unavailable, never followed, including same-origin redirects. No retry,
/// cache API, logging, inherited browser headers or configurable URL is used.
pub(super) async fn exchange(request: &ApiRequest) -> Exchange {
    let head = Cell::new(None);
    let operation = async {
        let headers = Headers::new();
        headers
            .set("Authorization", &request.authorization())
            .map_err(|_| WireError::Unavailable)?;
        headers
            .set("Accept", "application/json")
            .map_err(|_| WireError::Unavailable)?;
        headers
            .set("Cache-Control", "no-store")
            .map_err(|_| WireError::Unavailable)?;
        let method = match request.method() {
            "GET" => Method::Get,
            "POST" => Method::Post,
            _ => return Err(WireError::Unavailable),
        };
        let mut init = RequestInit::new();
        init.with_method(method)
            .with_redirect(redirect_policy())
            .with_cache(worker::CacheMode::NoStore);
        if let Some(body) = request.body() {
            headers
                .set("Content-Type", "application/json")
                .map_err(|_| WireError::Unavailable)?;
            init.with_body(Some(body.into()));
        }
        init.with_headers(headers);
        let outgoing =
            Request::new_with_init(request.url(), &init).map_err(|_| WireError::Unavailable)?;
        let controller = worker::AbortController::default();
        let signal = controller.signal();
        let _abort = AbortOnDrop(Some(controller));
        let mut response = Fetch::Request(outgoing)
            .send_with_signal(&signal)
            .await
            .map_err(|_| WireError::Unavailable)?;
        let status = response.status_code();
        let now_s = worker::Date::now().as_millis() / 1000;
        let mut rate_headers = Vec::new();
        for name in [
            "retry-after",
            "x-ratelimit-remaining",
            "x-ratelimit-reset-in",
            "x-ratelimit-reset",
        ] {
            match response.headers().get(name) {
                Ok(Some(value)) => rate_headers.push((name, value)),
                Ok(None) => {}
                // An unreadable header must not authorize an earlier retry.
                Err(_) => {
                    head.set(Some(ResponseHead {
                        status,
                        cooldown_s: u64::MAX,
                    }));
                    return Err(WireError::Unavailable);
                }
            }
        }
        let borrowed: Vec<_> = rate_headers
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        head.set(Some(ResponseHead::new(now_s, status, &borrowed)));
        super::require_success(status)?;
        if request.url() != super::VALIDATE_TOKEN_URL {
            // Feedback acceptance depends only on the status. Do not collect
            // arbitrary feedback/error bodies; the guard aborts unread bytes.
            return Ok(Zeroizing::new(Vec::new()));
        }
        let declared_len = response
            .headers()
            .get("content-length")
            .map_err(|_| WireError::Unavailable)?
            .and_then(|value| value.trim().parse::<usize>().ok());
        // Check before even obtaining/polling the body stream.
        if declared_len.is_some_and(|len| len > MAX_VALIDATION_BODY_BYTES) {
            return Err(WireError::InvalidResponse);
        }
        let stream = response.stream().map_err(|_| WireError::Unavailable)?;
        validation_body(declared_len, stream).await
    };
    timed_exchange(&head, operation, TOTAL_TIMEOUT_MS).await
}

#[cfg(test)]
mod tests;
