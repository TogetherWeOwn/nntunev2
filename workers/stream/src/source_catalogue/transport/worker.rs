//! Qualification adapter only. Construction is inert; no product caller or
//! binding configuration is installed. Abort signaling is not live Worker proof.

use std::time::Duration;

use nntune_core::original_source::service::{Payload, PATH};
use worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{JsCast, JsValue},
    web_sys, Delay, Env, Headers, Method, Request, RequestInit, RequestRedirect, ResponseBody,
};

use super::{Client, Head, Reply};
use crate::{account_playback::CatalogueUnavailable, source_catalogue::SourceTransport};

pub struct WorkerSourceTransport<'a>(pub &'a Env);

impl SourceTransport for WorkerSourceTransport<'_> {
    async fn fetch(
        &self,
        authorization: &str,
        body: &[u8],
        deadline_ms: u64,
    ) -> Result<Payload, CatalogueUnavailable> {
        let remaining = deadline_ms.saturating_sub(worker::Date::now().as_millis());
        super::fetch(
            &Binding(self.0),
            authorization,
            body,
            deadline_ms,
            || worker::Date::now().as_millis(),
            Delay::from(Duration::from_millis(remaining)),
        )
        .await
    }
}

struct Abort(web_sys::AbortController);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Binding<'a>(&'a Env);
struct BindingReply {
    head: Head,
    reader: web_sys::ReadableStreamDefaultReader,
    _abort: Abort,
}

impl Client for Binding<'_> {
    type Reply = BindingReply;
    async fn post(
        &self,
        authorization: &str,
        body: &[u8],
    ) -> Result<BindingReply, CatalogueUnavailable> {
        let api = self.0.service("API").map_err(|_| CatalogueUnavailable)?;
        let abort = Abort(web_sys::AbortController::new().map_err(|_| CatalogueUnavailable)?);
        let headers = Headers::new();
        for (name, value) in [
            ("authorization", authorization),
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ] {
            headers.set(name, value).map_err(|_| CatalogueUnavailable)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error)
            .with_body(Some(JsValue::from_str(
                std::str::from_utf8(body).map_err(|_| CatalogueUnavailable)?,
            )));
        let raw: web_sys::RequestInit = (&init).into();
        raw.set_signal(Some(&abort.0.signal()));
        // Only Fetcher on the API binding, never global/public fetch.
        let request =
            web_sys::Request::new_with_str_and_init(&format!("https://nntune-api{PATH}"), &raw)
                .map_err(|_| CatalogueUnavailable)?;
        let response = api
            .fetch_request(Request::from(request))
            .await
            .map_err(|_| CatalogueUnavailable)?;
        let headers = response.headers();
        let head = Head {
            status: response.status_code(),
            content_type: header(headers, "content-type", 24)?.unwrap_or_default(),
            cache_control: header(headers, "cache-control", 8)?.unwrap_or_default(),
            source_job_id: header(headers, "x-nntune-source-job", 64)?.unwrap_or_default(),
            content_length: header(headers, "content-length", 10)?,
            content_encoding: header(headers, "content-encoding", 8)?,
        };
        head.validate()?;
        let ResponseBody::Stream(stream) = response.body() else {
            return Err(CatalogueUnavailable);
        };
        let reader = stream
            .get_reader()
            .dyn_into::<web_sys::ReadableStreamDefaultReader>()
            .map_err(|_| CatalogueUnavailable)?;
        Ok(BindingReply {
            head,
            reader,
            _abort: abort,
        })
    }
}

fn header(
    headers: &Headers,
    name: &str,
    limit: u32,
) -> Result<Option<String>, CatalogueUnavailable> {
    // Headers::get returns String and would copy unbounded input before checking.
    let get = Reflect::get(headers.as_ref(), &JsValue::from_str("get"))
        .map_err(|_| CatalogueUnavailable)?
        .dyn_into::<js_sys::Function>()
        .map_err(|_| CatalogueUnavailable)?;
    let value = get
        .call1(headers.as_ref(), &JsValue::from_str(name))
        .map_err(|_| CatalogueUnavailable)?;
    if value.is_null() {
        return Ok(None);
    }
    let value = value
        .dyn_into::<js_sys::JsString>()
        .map_err(|_| CatalogueUnavailable)?;
    super::copy_header(value.length(), limit, || value.as_string()).map(Some)
}

impl Reply for BindingReply {
    fn head(&self) -> &Head {
        &self.head
    }

    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, CatalogueUnavailable> {
        let result = js_sys::futures::JsFuture::from(self.reader.read())
            .await
            .map_err(|_| CatalogueUnavailable)?;
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|_| CatalogueUnavailable)?
            .as_bool()
            .ok_or(CatalogueUnavailable)?;
        if done {
            return Ok(None);
        }
        let chunk = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|_| CatalogueUnavailable)?
            .dyn_into::<Uint8Array>()
            .map_err(|_| CatalogueUnavailable)?;
        if chunk.length() as usize > remaining {
            return Err(CatalogueUnavailable);
        }
        Ok(Some(chunk.to_vec()))
    }
}
