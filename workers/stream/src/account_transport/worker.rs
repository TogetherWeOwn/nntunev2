//! Internal binding-only adapter. Abort/cancel initiation is not confirmation
//! of remote work stopping or physical provider closure/capacity release.

use worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{JsCast, JsValue},
    web_sys, Env, Headers, Method, Request, RequestInit, RequestRedirect, ResponseBody,
};

use super::{Client, Head, Kind, Reply};
use crate::account_route::RegistryError;
use nntune_core::pool_channel::{CANDIDATE_PATH, OPERATION_PATH, REGISTRY_PATH, RESOLVE_PATH};
use zeroize::Zeroizing;

pub(crate) enum Binding<'a> {
    Api {
        env: &'a Env,
        kind: Kind,
    },
    Account {
        env: &'a Env,
        account_id: &'a str,
        kind: Kind,
    },
}

struct Abort(web_sys::AbortController);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(crate) struct BindingReply {
    head: Head,
    reader: Option<web_sys::ReadableStreamDefaultReader>,
    _abort: Abort,
}

impl Client for Binding<'_> {
    type Reply = BindingReply;

    async fn post(&self, authorization: &str, body: &[u8]) -> Result<BindingReply, RegistryError> {
        let path = match self {
            Self::Api {
                kind: Kind::Registry,
                ..
            } => REGISTRY_PATH,
            Self::Api {
                kind: Kind::Resolve,
                ..
            } => RESOLVE_PATH,
            Self::Account {
                kind: Kind::Candidate,
                ..
            } => CANDIDATE_PATH,
            Self::Account {
                kind: Kind::Operation,
                ..
            } => OPERATION_PATH,
            _ => return Err(RegistryError),
        };
        let abort = Abort(web_sys::AbortController::new().map_err(|_| RegistryError)?);
        let headers = Headers::new();
        for (name, value) in [
            ("authorization", authorization),
            ("content-type", "application/json"),
            ("cache-control", "no-store"),
        ] {
            headers.set(name, value).map_err(|_| RegistryError)?;
        }
        let mut init = RequestInit::new();
        init.with_method(Method::Post)
            .with_headers(headers)
            .with_redirect(RequestRedirect::Error)
            .with_body(Some(JsValue::from_str(
                std::str::from_utf8(body).map_err(|_| RegistryError)?,
            )));
        let raw: web_sys::RequestInit = (&init).into();
        raw.set_signal(Some(&abort.0.signal()));
        // No public/global fetch, redirect, retry or cache fallback.
        let response = match self {
            Self::Api { env, .. } => {
                let binding = env.service("API").map_err(|_| RegistryError)?;
                let request = web_sys::Request::new_with_str_and_init(
                    &format!("https://nntune-api{path}"),
                    &raw,
                )
                .map_err(|_| RegistryError)?;
                binding
                    .fetch_request(Request::from(request))
                    .await
                    .map_err(|_| RegistryError)?
            }
            Self::Account {
                env, account_id, ..
            } => {
                if !crate::account_pool::uuid_shaped(account_id) {
                    return Err(RegistryError);
                }
                let stub = env
                    .durable_object("NNTP_ACCOUNT")
                    .and_then(|ns| ns.id_from_name(account_id)?.get_stub())
                    .map_err(|_| RegistryError)?;
                let request = web_sys::Request::new_with_str_and_init(
                    &format!("https://nntune-account{path}"),
                    &raw,
                )
                .map_err(|_| RegistryError)?;
                stub.fetch_with_request(Request::from(request))
                    .await
                    .map_err(|_| RegistryError)?
            }
        };
        let status = response.status_code();
        let mut head = Head {
            status,
            content_type: String::new(),
            provider_id: None,
            cache_control: String::new(),
            content_length: None,
            content_encoding: None,
        };
        let reader = if status == 200 {
            let headers = response.headers();
            head.content_type = header(headers, "content-type", 32)?.unwrap_or_default();
            head.provider_id = header(headers, crate::account_dispatch::PROVIDER_HEADER, 36)?;
            head.cache_control = header(headers, "cache-control", 32)?.unwrap_or_default();
            head.content_length = header(headers, "content-length", 10)?;
            head.content_encoding = header(headers, "content-encoding", 8)?;
            let ResponseBody::Stream(stream) = response.body() else {
                return Err(RegistryError);
            };
            // The typed get_reader binding can throw outside Rust. Catch locked
            // stream exceptions so the pending-fetch abort guard always unwinds.
            let get_reader = Reflect::get(stream.as_ref(), &"getReader".into())
                .map_err(|_| RegistryError)?
                .dyn_into::<js_sys::Function>()
                .map_err(|_| RegistryError)?;
            Some(
                get_reader
                    .call0(stream.as_ref())
                    .map_err(|_| RegistryError)?
                    .dyn_into::<web_sys::ReadableStreamDefaultReader>()
                    .map_err(|_| RegistryError)?,
            )
        } else {
            None
        };
        Ok(BindingReply {
            head,
            reader,
            _abort: abort,
        })
    }
}

fn header(headers: &Headers, name: &str, limit: u32) -> Result<Option<String>, RegistryError> {
    // Headers::get copies to Rust before a length check; use raw UTF-16 length.
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
    let value = value
        .dyn_into::<js_sys::JsString>()
        .map_err(|_| RegistryError)?;
    super::copy_header(value.length(), limit, || value.as_string()).map(Some)
}

impl Reply for BindingReply {
    fn head(&self) -> &Head {
        &self.head
    }

    async fn next(
        &mut self,
        remaining: usize,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, RegistryError> {
        let reader = self.reader.as_ref().ok_or(RegistryError)?;
        let result = js_sys::futures::JsFuture::from(reader.read())
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
        super::copy_chunk(chunk.length(), remaining, || chunk.to_vec()).map(Some)
    }
}

impl Drop for BindingReply {
    fn drop(&mut self) {
        if let Some(reader) = &self.reader {
            // Initiate cancellation synchronously, never spawn a cleanup task.
            let _ = reader.cancel();
        }
        // _abort then signals the original fetch even during a pending read.
    }
}
