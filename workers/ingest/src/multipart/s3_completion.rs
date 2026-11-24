//! Signed multipart completion against the trusted ingest provider and bucket.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use nntune_core::s3_signing::{encode_query_value, S3SigningKey};
use sha2::{Digest, Sha256};
use worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{JsCast, JsValue},
    wasm_bindgen_futures::JsFuture,
    web_sys::{
        AbortController, Headers, ReadableStreamDefaultReader, Request, RequestCache, RequestInit,
        RequestRedirect, Response, WorkerGlobalScope,
    },
    Date, Delay, Url,
};

use super::{
    session_completion::{CompletionRequest, CompletionResponse, DispatchError, SessionCompletion},
    CompletedVersionedMedia, MAX_COMPLETION_BODY,
};
use crate::pool_deadline::race_against_deadline;

const CONTENT_TYPE: &str = "application/xml";
const SIGNED_HEADERS: &str = "content-type;host;x-amz-content-sha256;x-amz-date";

pub struct S3CompletionSender {
    configuration: [u8; 16],
    bucket: String,
    endpoint: Url,
    signing: S3SigningKey,
}

impl S3CompletionSender {
    /// Load a canonical HTTPS origin and write credentials from trusted configuration.
    pub fn new(
        configuration: [u8; 16],
        bucket: String,
        endpoint: &str,
        region: String,
        access_key: String,
        secret_key: String,
    ) -> Result<Self, DispatchError> {
        let signing = S3SigningKey::new(region, access_key, secret_key)
            .ok_or(DispatchError::InvalidRequest)?;
        let url = Url::parse(endpoint).map_err(|_| DispatchError::InvalidRequest)?;
        if url.scheme() != "https"
            || endpoint.strip_suffix('/').unwrap_or(endpoint) != url.origin().ascii_serialization()
        {
            return Err(DispatchError::InvalidRequest);
        }
        Ok(Self {
            configuration,
            bucket,
            endpoint: url,
            signing,
        })
    }

    /// The caller authorizes the live session and supplies its dispatch deadline.
    pub async fn complete(
        &self,
        completion: SessionCompletion<'_>,
        usable_until: SystemTime,
    ) -> Result<CompletedVersionedMedia, DispatchError> {
        completion
            .dispatch(async |request| self.send(request, usable_until).await)
            .await
    }

    async fn send(
        &self,
        request: CompletionRequest<'_>,
        until: SystemTime,
    ) -> Result<CompletionResponse, ()> {
        if self.configuration != request.configuration
            || self.bucket != request.target.bucket
            || now() >= until
        {
            return Err(());
        }
        let path = format!("/{}/{}", self.bucket, request.target.key);
        let query = format!("uploadId={}", encode_query_value(request.target.upload_id));
        let host =
            &self.endpoint[worker::url::Position::BeforeHost..worker::url::Position::AfterPort];
        let iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .ok_or(())?;
        let date = format!("{}Z", iso.get(..19).ok_or(())?.replace(['-', ':'], ""));
        let payload = hex::encode(Sha256::digest(&request.body));
        let canonical = format!("POST\n{path}\n{query}\ncontent-type:{CONTENT_TYPE}\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{date}\n\n{SIGNED_HEADERS}\n{payload}");
        let authorization = self
            .signing
            .authorization(&date, &canonical, SIGNED_HEADERS)
            .ok_or(())?;
        let headers = Headers::new().map_err(|_| ())?;
        for (name, value) in [
            ("content-type", CONTENT_TYPE),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", date.as_str()),
            ("authorization", authorization.as_str()),
            ("accept-encoding", "identity"),
        ] {
            headers.set(name, value).map_err(|_| ())?;
        }
        let abort = AbortOnDrop(AbortController::new().map_err(|_| ())?);
        let init = RequestInit::new();
        init.set_method("POST");
        init.set_headers(headers.as_ref());
        init.set_body_opt_u8_array(Some(&Uint8Array::from(request.body.as_slice())));
        init.set_redirect(RequestRedirect::Manual);
        init.set_cache(RequestCache::NoStore);
        init.set_signal(Some(&abort.0.signal()));
        if !Reflect::set(
            init.as_ref(),
            &"encodeResponseBody".into(),
            &"manual".into(),
        )
        .map_err(|_| ())?
        {
            return Err(());
        }
        let url = format!(
            "{}{path}?{query}",
            self.endpoint.origin().ascii_serialization()
        );
        let request = Request::new_with_str_and_init(&url, &init).map_err(|_| ())?;
        let wait = until.duration_since(now()).map_err(|_| ())?;
        if !(1..=i32::MAX as u128).contains(&wait.as_millis()) {
            return Err(());
        }
        let mut timer = Delay::from(wait);
        let reply = race_against_deadline(
            Box::pin(fetch_reply(&request, self.configuration)),
            &mut timer,
        )
        .await
        .ok_or(())??;
        (now() < until).then_some(reply).ok_or(())
    }
}

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(Date::now().as_millis())
}

struct AbortOnDrop(AbortController);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn fetch_reply(request: &Request, configuration: [u8; 16]) -> Result<CompletionResponse, ()> {
    let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
    let response: Response = JsFuture::from(scope.fetch_with_request(request))
        .await
        .map_err(|_| ())?
        .dyn_into()
        .map_err(|_| ())?;
    let status = response.status();
    let headers = response.headers();
    if response.redirected()
        || (300..400).contains(&status)
        || headers.has("content-encoding").map_err(|_| ())?
    {
        return Err(());
    }
    let version = headers.get("x-amz-version-id").map_err(|_| ())?;
    let mut body = Vec::new();
    if let Some(stream) = response.body() {
        let reader = ReadableStreamDefaultReader::new(&stream).map_err(|_| ())?;
        loop {
            let chunk = JsFuture::from(reader.read()).await.map_err(|_| ())?;
            let done = Reflect::get(&chunk, &"done".into())
                .map_err(|_| ())?
                .as_bool()
                .ok_or(())?;
            if done {
                break;
            }
            let bytes: Uint8Array = Reflect::get(&chunk, &"value".into())
                .map_err(|_| ())?
                .dyn_into()
                .map_err(|_| ())?;
            let start = body.len();
            let len = bytes.length() as usize;
            if len > MAX_COMPLETION_BODY - start {
                return Err(());
            }
            body.resize(start + len, 0);
            bytes.copy_to(&mut body[start..]);
        }
    }
    Ok(CompletionResponse {
        configuration,
        status,
        body,
        version,
    })
}

#[cfg(feature = "s3-completion-wasm-test")]
#[path = "../../tests/fixtures/s3_completion_wasm.rs"]
mod wasm_test;
