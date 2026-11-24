//! Signed multipart requests against the trusted ingest provider and bucket.

use std::{
    fmt::Write as _,
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use nntune_core::{
    media_envelope::EnvelopeContext,
    origin_sealer::MAX_NEW_PLAINTEXT,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
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
    head_receipt::{HeadReceipt, HeadReceiptError},
    resume_checkpoint::ResumeBinding,
    session_completion::{CompletionRequest, CompletionResponse, DispatchError, SessionCompletion},
    session_outcome::CompletionScope,
    session_upload::{self, DurableUpload, HandoffError},
    sidecar::{CompletedSidecar, SidecarError, SidecarPut, SidecarPutReply},
    CompletedVersionedMedia, MultipartError, UploadPart, UploadedPart, MAX_COMPLETION_BODY,
};
use crate::pool_deadline::race_against_deadline;

use super::initiation::InitiatedUpload;

#[path = "s3_readback.rs"]
mod readback;

#[derive(Debug, PartialEq, Eq)]
pub enum HeadConfirmationError {
    InvalidRequest,
    Transport,
    InvalidResponse,
    Receipt(HeadReceiptError),
}

/// The caller must reconcile the sidecar write before sending it again.
#[derive(Debug, PartialEq, Eq)]
pub enum SidecarDispatchError {
    Transport,
    Receipt(SidecarError),
}

pub struct S3CompletionSender {
    configuration: [u8; 16],
    bucket: String,
    endpoint: Url,
    signing: S3SigningKey,
}

impl S3CompletionSender {
    /// Load a canonical HTTPS origin and credentials from trusted configuration.
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

    /// Open or recover the durable target before constructing a part plan.
    /// Supply the authorized scope, trusted sealer intent, current lease and
    /// its caller deadline on a dedicated writer connection.
    pub async fn open_upload<'a>(
        &'a self,
        db: &mut tokio_postgres::Client,
        scope: &CompletionScope<'a>,
        lease: &str,
        context: &'a EnvelopeContext,
        key: &'a str,
        usable_until: SystemTime,
    ) -> Result<DurableUpload<'a>, HandoffError> {
        if self.configuration != context.identities[5] || now() >= usable_until {
            return Err(HandoffError::Refused);
        }
        let upload = session_upload::handoff(
            db,
            scope,
            lease,
            context,
            (&self.bucket, key),
            usable_until,
            async |deadline| {
                self.initiate(context, &self.bucket, key, deadline)
                    .await
                    .map_err(|_| ())
            },
        )
        .await?;
        if now() >= upload.usable_until() {
            return Err(HandoffError::Refused);
        }
        Ok(upload)
    }

    async fn initiate<'a>(
        &self,
        context: &EnvelopeContext,
        bucket: &'a str,
        key: &'a str,
        usable_until: SystemTime,
    ) -> Result<InitiatedUpload<'a>, DispatchError> {
        if !(1..=MAX_NEW_PLAINTEXT).contains(&context.plaintext_len)
            || !S3MediaMetadata::matches_target(bucket, key, context)
        {
            return Err(DispatchError::InvalidRequest);
        }
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: context.identities[5],
                    bucket,
                    key,
                    method: "POST",
                    query: "uploads=".into(),
                    body: &[],
                    payload_hash: hex::encode(Sha256::digest([])),
                    operation_headers: &[
                        ("x-amz-checksum-algorithm", "SHA256"),
                        ("x-amz-checksum-type", "COMPOSITE"),
                    ],
                },
                usable_until,
            )
            .await
            .map_err(|()| DispatchError::Unconfirmed(None))?;
        InitiatedUpload::verify(self.configuration, bucket, key, reply.status, &reply.body)
            .map_err(|error| DispatchError::Unconfirmed(Some(error)))
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

    /// Confirm metadata for the immutable completed version. Publication still
    /// requires authenticated ciphertext readback.
    pub async fn confirm_head(
        &self,
        completed: &CompletedVersionedMedia,
        context: &EnvelopeContext,
        usable_until: SystemTime,
    ) -> Result<(), HeadConfirmationError> {
        use HeadConfirmationError::{InvalidRequest, InvalidResponse, Receipt, Transport};

        let metadata = completed.metadata();
        let locator = metadata.locator();
        if !metadata.matches_context(context) {
            return Err(InvalidRequest);
        }
        let S3Binding::VersionId(version) = locator.binding() else {
            return Err(InvalidRequest);
        };
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: context.identities[5],
                    bucket: locator.bucket(),
                    key: locator.key(),
                    method: "HEAD",
                    query: format!("versionId={}", encode_query_value(version)),
                    body: &[],
                    payload_hash: hex::encode(Sha256::digest([])),
                    operation_headers: &[],
                },
                usable_until,
            )
            .await
            .map_err(|()| Transport)?;
        if reply.status != 200
            || !reply.body.is_empty()
            || reply
                .headers
                .has("content-range")
                .map_err(|_| InvalidResponse)?
        {
            return Err(InvalidResponse);
        }
        let observed_locator = reply
            .headers
            .get("x-amz-version-id")
            .map_err(|_| InvalidResponse)?
            .map(|observed| {
                if observed.contains(',') {
                    return Err(InvalidResponse);
                }
                S3Locator::new(
                    self.bucket.clone(),
                    locator.key().into(),
                    S3Binding::VersionId(observed),
                )
                .map_err(|_| InvalidResponse)
            })
            .transpose()?;
        let content_length = content_length(&reply.headers).map_err(|()| InvalidResponse)?;
        completed
            .verify_head(
                context,
                Some(HeadReceipt {
                    configuration: Some(self.configuration),
                    locator: observed_locator.as_ref(),
                    content_length,
                }),
            )
            .map_err(Receipt)
    }

    /// Write one sealer-derived digest page and capture the returned version
    /// for authenticated readback. The caller authorizes the live session.
    pub async fn put_sidecar<'a>(
        &self,
        put: SidecarPut<'a>,
        usable_until: SystemTime,
    ) -> Result<CompletedSidecar<'a>, SidecarDispatchError> {
        let (target, body) = put.request();
        let key = target.key.clone();
        let digest: [u8; 32] = Sha256::digest(body).into();
        let checksum = STANDARD.encode(digest);
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: target.context.identities[5],
                    bucket: target.bucket,
                    key: &key,
                    method: "PUT",
                    query: String::new(),
                    body,
                    payload_hash: hex::encode(digest),
                    operation_headers: &[("x-amz-checksum-sha256", &checksum)],
                },
                usable_until,
            )
            .await
            .map_err(|()| SidecarDispatchError::Transport)?;
        let version = reply
            .headers
            .get("x-amz-version-id")
            .map_err(|_| SidecarDispatchError::Receipt(SidecarError::Version))?;
        put.verify_reply(SidecarPutReply {
            configuration: self.configuration,
            bucket: &self.bucket,
            key: &key,
            status: reply.status,
            body: &reply.body,
            version_headers: version.as_deref().as_slice(),
        })
        .map_err(SidecarDispatchError::Receipt)
    }

    /// Upload a locally sealed part to the committed target. The caller must
    /// still own the checkpoint lease and retain exact ciphertext for recovery.
    pub async fn put_part<'a>(
        &self,
        binding: ResumeBinding<'a>,
        context: &EnvelopeContext,
        part: &UploadPart<'a>,
        upload: &DurableUpload<'_>,
        usable_until: SystemTime,
    ) -> Result<UploadedPart<'a>, DispatchError> {
        let (target, number, body, checksum) = part.request();
        if part.plan != binding.plan
            || !binding.matches_context(context)
            || !upload.matches(binding, context)
        {
            return Err(DispatchError::InvalidRequest);
        }
        let reply = self
            .signed_request(
                SignedRequest {
                    configuration: binding.configuration,
                    bucket: target.bucket,
                    key: target.key,
                    method: "PUT",
                    query: format!(
                        "partNumber={number}&uploadId={}",
                        encode_query_value(target.upload_id)
                    ),
                    body,
                    payload_hash: hex::encode(part.digest),
                    operation_headers: &[("x-amz-checksum-sha256", checksum)],
                },
                usable_until.min(upload.usable_until()),
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
            .signed_request(
                SignedRequest {
                    configuration: request.configuration,
                    bucket: request.target.bucket,
                    key: request.target.key,
                    method: "POST",
                    query: format!("uploadId={}", encode_query_value(request.target.upload_id)),
                    body: &request.body,
                    payload_hash: hex::encode(Sha256::digest(&request.body)),
                    operation_headers: &[("content-type", "application/xml")],
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

    async fn signed_request(
        &self,
        request: SignedRequest<'_>,
        until: SystemTime,
    ) -> Result<SignedReply, ()> {
        let (request, mut deadline) = self.prepare_request(request, until)?;
        deadline.run(fetch_reply(&request)).await
    }

    fn prepare_request(
        &self,
        request: SignedRequest<'_>,
        until: SystemTime,
    ) -> Result<(Request, RequestDeadline), ()> {
        if self.configuration != request.configuration
            || self.bucket != request.bucket
            || now() >= until
        {
            return Err(());
        }
        let path = format!("/{}/{}", self.bucket, request.key);
        let host =
            &self.endpoint[worker::url::Position::BeforeHost..worker::url::Position::AfterPort];
        let iso = js_sys::Date::new_0()
            .to_iso_string()
            .as_string()
            .ok_or(())?;
        let date = format!("{}Z", iso.get(..19).ok_or(())?.replace(['-', ':'], ""));
        let payload = &request.payload_hash;
        let mut signed = vec![
            ("host", host),
            ("x-amz-content-sha256", payload.as_str()),
            ("x-amz-date", date.as_str()),
        ];
        signed.extend_from_slice(request.operation_headers);
        signed.sort_unstable_by_key(|(name, _)| *name);
        let names = signed
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>()
            .join(";");
        let mut canonical = format!("{}\n{path}\n{}\n", request.method, request.query);
        for (name, value) in &signed {
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
        if !request.body.is_empty() {
            init.set_body_opt_u8_array(Some(&Uint8Array::from(request.body)));
        }
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
        let mut url = format!("{}{path}", self.endpoint.origin().ascii_serialization());
        if !request.query.is_empty() {
            url.push('?');
            url.push_str(&request.query);
        }
        let request = Request::new_with_str_and_init(&url, &init).map_err(|_| ())?;
        let wait = until.duration_since(now()).map_err(|_| ())?;
        if !(1..=i32::MAX as u128).contains(&wait.as_millis()) {
            return Err(());
        }
        Ok((
            request,
            RequestDeadline {
                until,
                timer: Delay::from(wait),
                _abort: abort,
            },
        ))
    }
}

struct SignedRequest<'a> {
    configuration: [u8; 16],
    bucket: &'a str,
    key: &'a str,
    method: &'static str,
    query: String,
    body: &'a [u8],
    payload_hash: String,
    operation_headers: &'a [(&'static str, &'a str)],
}

struct SignedReply {
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

struct RequestDeadline {
    until: SystemTime,
    timer: Delay,
    _abort: AbortOnDrop,
}

impl RequestDeadline {
    async fn run<T>(&mut self, operation: impl Future<Output = Result<T, ()>>) -> Result<T, ()> {
        self.check()?;
        let result = race_against_deadline(Box::pin(operation), &mut self.timer)
            .await
            .ok_or(())??;
        self.check()?;
        Ok(result)
    }

    fn check(&self) -> Result<(), ()> {
        (now() < self.until).then_some(()).ok_or(())
    }
}

fn content_length(headers: &Headers) -> Result<Option<u64>, ()> {
    headers
        .get("content-length")
        .map_err(|_| ())?
        .map(|length| {
            if !length.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(());
            }
            length.parse().map_err(|_| ())
        })
        .transpose()
}

async fn fetch_response(request: &Request) -> Result<Response, ()> {
    let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
    let response: Response = JsFuture::from(scope.fetch_with_request(request))
        .await
        .map_err(|_| ())?
        .dyn_into()
        .map_err(|_| ())?;
    if response.redirected()
        || (300..400).contains(&response.status())
        || response.headers().has("content-encoding").map_err(|_| ())?
    {
        return Err(());
    }
    Ok(response)
}

async fn next_response_chunk(
    reader: &ReadableStreamDefaultReader,
) -> Result<Option<Uint8Array>, ()> {
    let chunk = JsFuture::from(reader.read()).await.map_err(|_| ())?;
    if Reflect::get(&chunk, &"done".into())
        .map_err(|_| ())?
        .as_bool()
        .ok_or(())?
    {
        return Ok(None);
    }
    Reflect::get(&chunk, &"value".into())
        .map_err(|_| ())?
        .dyn_into()
        .map(Some)
        .map_err(|_| ())
}

async fn fetch_reply(request: &Request) -> Result<SignedReply, ()> {
    let response = fetch_response(request).await?;
    let mut body = Vec::new();
    if let Some(stream) = response.body() {
        let reader = ReadableStreamDefaultReader::new(&stream).map_err(|_| ())?;
        while let Some(bytes) = next_response_chunk(&reader).await? {
            let start = body.len();
            let len = bytes.length() as usize;
            if len > MAX_COMPLETION_BODY - start {
                return Err(());
            }
            body.resize(start + len, 0);
            bytes.copy_to(&mut body[start..]);
        }
    }
    Ok(SignedReply {
        status: response.status(),
        headers: response.headers(),
        body,
    })
}

#[cfg(feature = "s3-completion-wasm-test")]
#[path = "../../tests/fixtures/s3_completion_wasm.rs"]
mod wasm_test;
