//! Signed multipart writes against the trusted ingest provider and bucket.

use std::{
    fmt::Write as _,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nntune_core::{
    media_envelope::EnvelopeContext,
    s3_signing::{encode_query_value, S3SigningKey},
};
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
    resume_checkpoint::ResumeBinding,
    session_completion::{
        valid_upload_id, CompletionRequest, CompletionResponse, DispatchError, SessionCompletion,
    },
    CompletedVersionedMedia, MultipartError, UploadPart, UploadTarget, UploadedPart,
    MAX_COMPLETION_BODY,
};
use crate::pool_deadline::race_against_deadline;

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

    /// Upload only the locally sealed part from the authorized session. Retain
    /// those immutable bytes and reconcile the session after an unconfirmed send.
    pub async fn put_part<'a>(
        &self,
        binding: ResumeBinding<'a>,
        context: &EnvelopeContext,
        part: &UploadPart<'a>,
        usable_until: SystemTime,
    ) -> Result<UploadedPart<'a>, DispatchError> {
        let (target, number, body, checksum) = part.request();
        if part.plan != binding.plan
            || !binding.matches_context(context)
            || !valid_upload_id(target.upload_id)
        {
            return Err(DispatchError::InvalidRequest);
        }
        let reply = self
            .write_request(
                WriteRequest {
                    configuration: binding.configuration,
                    target,
                    method: "PUT",
                    query: format!(
                        "partNumber={number}&uploadId={}",
                        encode_query_value(target.upload_id)
                    ),
                    body,
                    payload_hash: hex::encode(part.digest),
                    operation_header: ("x-amz-checksum-sha256", checksum),
                },
                usable_until,
            )
            .await
            .map_err(|()| DispatchError::Unconfirmed(None))?;
        let refuse = || DispatchError::Unconfirmed(Some(MultipartError::PartReceipt));
        if reply.status != 200 || !reply.body.is_empty() {
            return Err(refuse());
        }
        let etag = reply
            .headers
            .get("etag")
            .map_err(|_| refuse())?
            .filter(|tag| {
                tag.strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                    .is_some_and(|value| {
                        !value.is_empty()
                            && value
                                .bytes()
                                .all(|byte| byte.is_ascii_graphic() && byte != b'"')
                    })
            })
            .ok_or_else(refuse)?;
        let returned = reply
            .headers
            .get("x-amz-checksum-sha256")
            .map_err(|_| refuse())?
            .ok_or_else(refuse)?;
        part.verify_reply(etag, &returned)
            .map_err(|error| DispatchError::Unconfirmed(Some(error)))
    }

    async fn send(
        &self,
        request: CompletionRequest<'_>,
        until: SystemTime,
    ) -> Result<CompletionResponse, ()> {
        let reply = self
            .write_request(
                WriteRequest {
                    configuration: request.configuration,
                    target: request.target,
                    method: "POST",
                    query: format!("uploadId={}", encode_query_value(request.target.upload_id)),
                    body: &request.body,
                    payload_hash: hex::encode(Sha256::digest(&request.body)),
                    operation_header: ("content-type", "application/xml"),
                },
                until,
            )
            .await?;
        Ok(CompletionResponse {
            configuration: self.configuration,
            status: reply.status,
            body: reply.body,
            version: reply.headers.get("x-amz-version-id").map_err(|_| ())?,
        })
    }

    async fn write_request(
        &self,
        request: WriteRequest<'_>,
        until: SystemTime,
    ) -> Result<WriteReply, ()> {
        if self.configuration != request.configuration
            || self.bucket != request.target.bucket
            || now() >= until
        {
            return Err(());
        }
        let path = format!("/{}/{}", self.bucket, request.target.key);
        let host =
            &self.endpoint[worker::url::Position::BeforeHost..worker::url::Position::AfterPort];
        let iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .ok_or(())?;
        let date = format!("{}Z", iso.get(..19).ok_or(())?.replace(['-', ':'], ""));
        let payload = &request.payload_hash;
        let mut signed = [
            ("host", host),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", date.as_str()),
            request.operation_header,
        ];
        signed.sort_unstable_by_key(|(name, _)| *name);
        let names = signed.map(|(name, _)| name).join(";");
        let mut canonical = format!("{}\n{path}\n{}\n", request.method, request.query);
        for (name, value) in signed {
            writeln!(canonical, "{name}:{value}").map_err(|_| ())?;
        }
        write!(canonical, "\n{names}\n{payload}").map_err(|_| ())?;
        let authorization = self
            .signing
            .authorization(&date, &canonical, &names)
            .ok_or(())?;
        let headers = Headers::new().map_err(|_| ())?;
        for (name, value) in signed {
            if name != "host" {
                headers.set(name, value).map_err(|_| ())?;
            }
        }
        headers
            .set("authorization", authorization.as_str())
            .map_err(|_| ())?;
        headers.set("accept-encoding", "identity").map_err(|_| ())?;
        let abort = AbortOnDrop(AbortController::new().map_err(|_| ())?);
        let init = RequestInit::new();
        init.set_method(request.method);
        init.set_headers(headers.as_ref());
        init.set_body_opt_u8_array(Some(&Uint8Array::from(request.body)));
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
            "{}{path}?{}",
            self.endpoint.origin().ascii_serialization(),
            request.query,
        );
        let request = Request::new_with_str_and_init(&url, &init).map_err(|_| ())?;
        let wait = until.duration_since(now()).map_err(|_| ())?;
        if !(1..=i32::MAX as u128).contains(&wait.as_millis()) {
            return Err(());
        }
        let mut timer = Delay::from(wait);
        let reply = race_against_deadline(Box::pin(fetch_reply(&request)), &mut timer)
            .await
            .ok_or(())??;
        (now() < until).then_some(reply).ok_or(())
    }
}

struct WriteRequest<'a> {
    configuration: [u8; 16],
    target: UploadTarget<'a>,
    method: &'static str,
    query: String,
    body: &'a [u8],
    payload_hash: String,
    operation_header: (&'static str, &'a str),
}

struct WriteReply {
    status: u16,
    headers: Headers,
    body: Vec<u8>,
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

async fn fetch_reply(request: &Request) -> Result<WriteReply, ()> {
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
    Ok(WriteReply {
        status,
        headers,
        body,
    })
}

#[cfg(feature = "s3-completion-wasm-test")]
#[path = "../../tests/fixtures/s3_completion_wasm.rs"]
mod wasm_test;
