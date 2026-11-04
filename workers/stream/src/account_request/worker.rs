//! Raw Worker request boundary. This does not enable account operations.

use std::{
    cell::RefCell,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context, Poll, Waker},
};

use ::worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{closure::Closure, JsCast, JsValue},
    web_sys, Headers, Method, Request,
};

use super::{Body, Head};
use crate::account_route::RegistryError;

// The SDK DO export has already copied the URL to construct Request's cached
// path. This bounds subsequent handler copies only, not that initial allocation.
// A pre-conversion ingress guard remains a separate qualification prerequisite.
pub(crate) fn path(req: &Request) -> Result<String, RegistryError> {
    let raw = bounded_string(
        Reflect::get(req.inner().as_ref(), &"url".into()).map_err(|_| RegistryError)?,
        256,
    )?;
    let url = ::worker::Url::parse(&raw).map_err(|_| RegistryError)?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(RegistryError);
    }
    Ok(url.path().to_owned())
}

pub(crate) fn internal_post(req: &Request) -> Result<(), RegistryError> {
    if req.method() != Method::Post {
        return Err(RegistryError);
    }
    for name in ["origin", "cookie", "sec-fetch-site", "sec-fetch-mode"] {
        if req.headers().has(name).map_err(|_| RegistryError)? {
            return Err(RegistryError);
        }
    }
    Ok(())
}

pub(crate) fn authorization(req: &Request) -> Result<Option<String>, RegistryError> {
    header(req.headers(), "authorization", 256)
}

pub(crate) async fn read(req: &Request, cap: usize) -> Result<Vec<u8>, RegistryError> {
    let headers = req.headers();
    let content_type = header(headers, "content-type", 24)?;
    let content_length = header(headers, "content-length", 10)?;
    let content_encoding = header(headers, "content-encoding", 16)?;
    let declared = Head {
        content_type: content_type.as_deref(),
        content_length: content_length.as_deref(),
        content_encoding: content_encoding.as_deref(),
    }
    .validate(cap)?;
    let stream = req.inner().body().ok_or(RegistryError)?;
    // Catch locked-body exceptions instead of unwinding past Rust guards.
    let get_reader = Reflect::get(stream.as_ref(), &"getReader".into())
        .map_err(|_| RegistryError)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| RegistryError)?;
    let reader = get_reader
        .call0(stream.as_ref())
        .map_err(|_| RegistryError)?
        .dyn_into::<web_sys::ReadableStreamDefaultReader>()
        .map_err(|_| RegistryError)?;
    super::collect(RequestBody { reader }, cap, declared).await
}

fn bounded_string(value: JsValue, limit: u32) -> Result<String, RegistryError> {
    let value = value
        .dyn_into::<js_sys::JsString>()
        .map_err(|_| RegistryError)?;
    if value.length() > limit {
        return Err(RegistryError);
    }
    let value = value.as_string().ok_or(RegistryError)?;
    if value.len() > limit as usize {
        return Err(RegistryError);
    }
    Ok(value)
}

fn header(headers: &Headers, name: &str, limit: u32) -> Result<Option<String>, RegistryError> {
    let get = Reflect::get(headers.as_ref(), &"get".into())
        .map_err(|_| RegistryError)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| RegistryError)?;
    let value = get
        .call1(headers.as_ref(), &JsValue::from_str(name))
        .map_err(|_| RegistryError)?;
    if value.is_null() {
        return Ok(None);
    }
    bounded_string(value, limit).map(Some)
}

struct RequestBody {
    reader: web_sys::ReadableStreamDefaultReader,
}

impl Body for RequestBody {
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, RegistryError> {
        let result = js_sys::futures::JsFuture::from(self.reader.read())
            .await
            .map_err(|_| RegistryError)?;
        let done = Reflect::get(&result, &"done".into())
            .map_err(|_| RegistryError)?
            .as_bool()
            .ok_or(RegistryError)?;
        if done {
            return Ok(None);
        }
        let chunk = Reflect::get(&result, &"value".into())
            .map_err(|_| RegistryError)?
            .dyn_into::<Uint8Array>()
            .map_err(|_| RegistryError)?;
        if chunk.length() == 0 || chunk.length() as usize > remaining {
            return Err(RegistryError);
        }
        Ok(Some(chunk.to_vec()))
    }
}

impl Drop for RequestBody {
    fn drop(&mut self) {
        // Initiation only, not remote-stop or physical socket closure evidence.
        let _ = self.reader.cancel();
    }
}

pub(crate) struct RequestAbort {
    signal: web_sys::AbortSignal,
    waker: Rc<RefCell<Option<Waker>>>,
    listener: Closure<dyn FnMut()>,
}

impl RequestAbort {
    pub fn new(req: &Request) -> Result<Self, RegistryError> {
        let signal = req.inner().signal();
        let waker: Rc<RefCell<Option<Waker>>> = Rc::default();
        let wake = waker.clone();
        let listener = Closure::new(move || {
            if let Some(waker) = wake.borrow_mut().take() {
                waker.wake();
            }
        });
        signal
            .add_event_listener_with_callback("abort", listener.as_ref().unchecked_ref())
            .map_err(|_| RegistryError)?;
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
