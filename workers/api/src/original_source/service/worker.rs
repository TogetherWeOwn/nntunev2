//! Off-route Worker source-handler candidate. No fetch dispatch or binding
//! configuration references this entry. Abort/close propagation needs actual
//! Worker qualification; compilation and native fakes do not establish it.

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
    time::Duration,
};

use ::worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{closure::Closure, JsCast, JsValue},
    web_sys, Delay, Env, Headers, Method, Request, Response,
};
use nntune_core::original_source::service::{MAX_LOOKUP_MS, PATH};

use super::http::{self, Body, Head};
use crate::{
    original_source::{postgres::owned::WorkerRepositoryFactory, SourceServiceError},
    rotation::KekSource,
};

/// Caller supplies the existing keys, never a new secret or a fallback key.
/// Database binding access remains lazy and occurs only after authentication.
pub async fn handle(
    request: Request,
    env: Env,
    keks: &impl KekSource,
    play_grant_key: Option<&[u8]>,
) -> ::worker::Result<Response> {
    match attempt(request, env, keks, play_grant_key).await {
        Ok(reply) => materialize(reply),
        Err(_) => unavailable(),
    }
}

async fn attempt(
    request: Request,
    env: Env,
    keks: &impl KekSource,
    play_grant_key: Option<&[u8]>,
) -> Result<http::Reply, SourceServiceError> {
    let cancellation = RequestAbort::new(request.inner().signal())?;
    let deadline = Delay::from(Duration::from_millis(MAX_LOOKUP_MS));
    let failure = || SourceServiceError::Denied;
    if request.method() != Method::Post {
        return Err(failure());
    }
    // Preflight the raw JS string before Request::url or URL parsing could copy
    // an unbounded caller-controlled URL into Rust.
    let url = bounded_string(
        Reflect::get(request.inner().as_ref(), &"url".into()).map_err(|_| failure())?,
        256,
    )?;
    let url = url::Url::parse(&url).map_err(|_| failure())?;
    if url.path() != PATH || url.query().is_some() || url.fragment().is_some() {
        return Err(failure());
    }
    let headers = request.headers();
    let browser_context = ["origin", "cookie", "sec-fetch-site", "sec-fetch-mode"]
        .into_iter()
        .try_fold(false, |present, name| {
            headers.has(name).map(|value| present || value)
        })
        .map_err(|_| failure())?;
    let authorization = header(headers, "authorization", 256)?;
    let content_type = header(headers, "content-type", 24)?;
    let content_length = header(headers, "content-length", 10)?;
    let content_encoding = header(headers, "content-encoding", 16)?;
    let stream = request.inner().body().ok_or_else(failure)?;
    // The typed get_reader binding does not catch JavaScript exceptions. A
    // locked body must unwind through Rust so the abort listener is removed.
    let get_reader = Reflect::get(stream.as_ref(), &"getReader".into())
        .map_err(|_| failure())?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| failure())?;
    let reader = get_reader
        .call0(stream.as_ref())
        .map_err(|_| failure())?
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| failure())?;
    let factory = WorkerRepositoryFactory::new(env);
    Ok(http::handle(
        &factory,
        keks,
        play_grant_key,
        Head {
            method: "POST",
            path: PATH,
            authorization: authorization.as_deref(),
            content_type: content_type.as_deref(),
            content_length: content_length.as_deref(),
            content_encoding: content_encoding.as_deref(),
            browser_context,
        },
        RequestBody { reader },
        || ::worker::Date::now().as_millis(),
        cancellation,
        deadline,
    )
    .await)
}

fn bounded_string(value: JsValue, limit: u32) -> Result<String, SourceServiceError> {
    let value = value
        .dyn_into::<js_sys::JsString>()
        .map_err(|_| SourceServiceError::Denied)?;
    if value.length() > limit {
        return Err(SourceServiceError::Denied);
    }
    let value = value.as_string().ok_or(SourceServiceError::Denied)?;
    if value.len() > limit as usize {
        return Err(SourceServiceError::Denied);
    }
    Ok(value)
}

fn header(headers: &Headers, name: &str, limit: u32) -> Result<Option<String>, SourceServiceError> {
    // Headers::get returns an already-copied Rust String; use raw JS instead.
    let get = Reflect::get(headers.as_ref(), &"get".into())
        .map_err(|_| SourceServiceError::Denied)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| SourceServiceError::Denied)?;
    let value = get
        .call1(headers.as_ref(), &JsValue::from_str(name))
        .map_err(|_| SourceServiceError::Denied)?;
    if value.is_null() {
        return Ok(None);
    }
    bounded_string(value, limit).map(Some)
}

struct RequestBody {
    reader: web_sys::ReadableStreamDefaultReader,
}

impl Body for RequestBody {
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, SourceServiceError> {
        let failure = || SourceServiceError::Denied;
        let result = js_sys::futures::JsFuture::from(self.reader.read())
            .await
            .map_err(|_| failure())?;
        let done = Reflect::get(&result, &"done".into())
            .map_err(|_| failure())?
            .as_bool()
            .ok_or_else(failure)?;
        if done {
            return Ok(None);
        }
        let chunk = Reflect::get(&result, &"value".into())
            .map_err(|_| failure())?
            .dyn_into::<Uint8Array>()
            .map_err(|_| failure())?;
        if chunk.length() as usize > remaining {
            return Err(failure());
        }
        Ok(Some(chunk.to_vec()))
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        // Synchronous cancellation initiation, not an acknowledged remote abort.
        let _ = self.reader.cancel();
    }
}

struct RequestAbort {
    signal: web_sys::AbortSignal,
    waker: Rc<RefCell<Option<Waker>>>,
    listener: Closure<dyn FnMut()>,
}

impl RequestAbort {
    fn new(signal: web_sys::AbortSignal) -> Result<Self, SourceServiceError> {
        let waker: Rc<RefCell<Option<Waker>>> = Rc::default();
        let wake = waker.clone();
        let listener = Closure::new(move || {
            if let Some(waker) = wake.borrow_mut().take() {
                waker.wake();
            }
        });
        signal
            .add_event_listener_with_callback("abort", listener.as_ref().unchecked_ref())
            .map_err(|_| SourceServiceError::Denied)?;
        Ok(Self {
            signal,
            waker,
            listener,
        })
    }
}

impl Future for RequestAbort {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.signal.aborted() {
            return Poll::Ready(());
        }
        *self.waker.borrow_mut() = Some(cx.waker().clone());
        if self.signal.aborted() {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

impl Drop for RequestAbort {
    fn drop(&mut self) {
        let _ = self
            .signal
            .remove_event_listener_with_callback("abort", self.listener.as_ref().unchecked_ref());
    }
}

fn materialize(reply: http::Reply) -> ::worker::Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", reply.content_type)?;
    headers.set("cache-control", reply.cache_control)?;
    headers.set("content-length", &reply.bytes.len().to_string())?;
    if let Some(job) = reply.source_job_id {
        headers.set("x-nntune-source-job", &job)?;
    }
    Ok(Response::from_bytes(reply.bytes)?
        .with_status(reply.status)
        .with_headers(headers))
}

fn unavailable() -> ::worker::Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", "application/octet-stream")?;
    headers.set("cache-control", "no-store")?;
    headers.set("content-length", "0")?;
    Ok(Response::empty()?.with_status(503).with_headers(headers))
}
