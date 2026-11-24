//! Off-route, version-pinned S3 GET over Worker Fetch.

use std::{
    fmt::Write as _,
    io,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use chrono::DateTime;
use nntune_core::{digest_readback::ReadbackBody, s3_locator::S3Binding};
use url::Url;
use worker::{
    js_sys::{self, Reflect, Uint8Array},
    wasm_bindgen::{closure::Closure, JsCast, JsValue},
    wasm_bindgen_futures::JsFuture,
    web_sys::{
        AbortController, Headers, ReadableStreamDefaultReader, Request, RequestCache, RequestInit,
        RequestRedirect, Response, WorkerGlobalScope,
    },
    Date,
};
use zeroize::Zeroizing;

use super::{S3Get, S3GetReply};
use crate::{
    grant::{hmac_sha256, sha256},
    s3_range_response::{validate_s3_range_metadata, S3RangeResponse},
};

const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const SIGNED_HEADERS: &str = "host;range;x-amz-content-sha256;x-amz-date";

/// One independently provisioned provider and bucket. Supply a read-only S3
/// credential with GetObjectVersion authority for the configured bucket.
pub struct S3Reader {
    configuration: [u8; 16],
    bucket: String,
    endpoint: Url,
    region: String,
    access_key: String,
    secret_key: Zeroizing<String>,
}

impl S3Reader {
    /// Accept a canonical HTTPS origin for path-style S3 addressing.
    pub fn new(
        configuration: [u8; 16],
        bucket: String,
        endpoint: &str,
        region: String,
        access_key: String,
        secret_key: Zeroizing<String>,
    ) -> io::Result<Self> {
        let url = Url::parse(endpoint).map_err(|_| refused())?;
        if url.scheme() != "https"
            || endpoint.strip_suffix('/').unwrap_or(endpoint) != url.origin().ascii_serialization()
            || !(1..=64).contains(&region.len())
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !(1..=128).contains(&access_key.len())
            || !access_key.bytes().all(|b| b.is_ascii_alphanumeric())
            || !(1..=256).contains(&secret_key.len())
            || !secret_key.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(refused());
        }
        Ok(Self {
            configuration,
            bucket,
            endpoint: url,
            region,
            access_key,
            secret_key,
        })
    }

    pub async fn open(&self, get: S3Get<'_>) -> io::Result<S3GetReply<S3Body>> {
        let S3Binding::VersionId(version) = get.locator.binding() else {
            return Err(refused());
        };
        if now() >= get.usable_until
            || self.configuration != get.configuration
            || self.bucket != get.locator.bucket()
            || version.contains(',')
            || get.range.start >= get.range.end
            || get.range.end > get.sealed_len
        {
            return Err(refused());
        }

        let path = format!("/{}/{}", self.bucket, get.locator.key());
        let query = format!("versionId={}", uri_component(version));
        let host = &self.endpoint[url::Position::BeforeHost..url::Position::AfterPort];
        let range = format!("bytes={}-{}", get.range.start, get.range.end - 1);
        let date = DateTime::from_timestamp((Date::now().as_millis() / 1000) as i64, 0)
            .ok_or_else(refused)?
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let authorization = self.authorization(host, &path, &query, &range, &date);
        let headers = Headers::new().map_err(|_| refused())?;
        for (name, value) in [
            ("range", range.as_str()),
            ("x-amz-content-sha256", EMPTY_HASH),
            ("x-amz-date", &date),
            ("authorization", &authorization),
            ("accept-encoding", "identity"),
        ] {
            headers.set(name, value).map_err(|_| refused())?;
        }

        let abort = Deadline::new(get.usable_until)?;
        let init = RequestInit::new();
        init.set_method("GET");
        init.set_headers(headers.as_ref());
        init.set_redirect(RequestRedirect::Manual);
        init.set_cache(RequestCache::NoStore);
        init.set_signal(Some(&abort.controller.signal()));
        // Workers extension. Inspect the wire representation before taking its stream.
        if !Reflect::set(
            init.as_ref(),
            &"encodeResponseBody".into(),
            &"manual".into(),
        )
        .map_err(|_| refused())?
        {
            return Err(refused());
        }
        let url = format!(
            "{}{path}?{query}",
            self.endpoint.origin().ascii_serialization()
        );
        let request = Request::new_with_str_and_init(&url, &init).map_err(|_| refused())?;
        abort.check()?;
        let response: Response = JsFuture::from(abort.scope.fetch_with_request(&request))
            .await
            .map_err(|_| refused())?
            .dyn_into()
            .map_err(|_| refused())?;
        abort.check()?;

        let status = response.status();
        let headers = response.headers();
        let header = |name| headers.get(name).map_err(|_| refused());
        if response.redirected()
            || status != 206
            || header("content-encoding")?.is_some()
            || header("transfer-encoding")?.is_some()
        {
            return Err(refused());
        }
        let content_range = header("content-range")?;
        let content_length = header("content-length")?
            .filter(|value| value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse().ok());
        let observed = header("x-amz-version-id")?.map(S3Binding::VersionId);
        // Fetch joins repeated fields with commas. Exact version/range and
        // decimal-only length validation reject joined values.
        let remaining = validate_s3_range_metadata(
            get.sealed_len,
            get.range,
            get.locator.binding(),
            S3RangeResponse {
                status,
                content_range: content_range.as_deref(),
                content_length,
                binding: observed.as_ref(),
            },
        )
        .map_err(|_| refused())?;
        let Some(S3Binding::VersionId(version)) = observed else {
            return Err(refused());
        };
        let stream = response.body().ok_or_else(refused)?;
        let reader = ReadableStreamDefaultReader::new(&stream).map_err(|_| refused())?;
        Ok(S3GetReply {
            status,
            version: Some(version),
            content_range,
            content_length,
            has_content_encoding: false,
            body: S3Body(Some(Body {
                abort,
                reader,
                chunk: None,
                remaining,
            })),
        })
    }

    fn authorization(
        &self,
        host: &str,
        path: &str,
        query: &str,
        range: &str,
        date: &str,
    ) -> String {
        let canonical = format!(
            "GET\n{path}\n{query}\nhost:{host}\nrange:{range}\nx-amz-content-sha256:{EMPTY_HASH}\nx-amz-date:{date}\n\n{SIGNED_HEADERS}\n{EMPTY_HASH}"
        );
        let day = &date[..8];
        let scope = format!("{day}/{}/s3/aws4_request", self.region);
        let digest = hex(&sha256(canonical.as_bytes()));
        let message = format!("AWS4-HMAC-SHA256\n{date}\n{scope}\n{digest}");
        let secret = Zeroizing::new(format!("AWS4{}", *self.secret_key));
        let key = Zeroizing::new(hmac_sha256(secret.as_bytes(), day.as_bytes()));
        let key = Zeroizing::new(hmac_sha256(key.as_ref(), self.region.as_bytes()));
        let key = Zeroizing::new(hmac_sha256(key.as_ref(), b"s3"));
        let key = Zeroizing::new(hmac_sha256(key.as_ref(), b"aws4_request"));
        let signature = hex(&hmac_sha256(key.as_ref(), message.as_bytes()));
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={SIGNED_HEADERS}, Signature={signature}",
            self.access_key
        )
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn refused() -> io::Error {
    io::Error::other("storage read refused")
}

fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(Date::now().as_millis())
}

/// Keep one abort signal and timer from dispatch until EOF or drop, including
/// time spent between reads while playback resolves pages or writes plaintext.
struct Deadline {
    controller: AbortController,
    scope: WorkerGlobalScope,
    timer: i32,
    _callback: Closure<dyn FnMut()>,
    until: SystemTime,
}

impl Deadline {
    fn new(until: SystemTime) -> io::Result<Self> {
        let delay = until
            .duration_since(now())
            .map_err(|_| refused())?
            .as_millis();
        let delay = i32::try_from(delay)
            .ok()
            .filter(|&ms| ms > 0)
            .ok_or_else(refused)?;
        let controller = AbortController::new().map_err(|_| refused())?;
        let abort = controller.clone();
        let callback = Closure::once(move || abort.abort());
        let scope: WorkerGlobalScope = js_sys::global().unchecked_into();
        let timer = scope
            .set_timeout_with_callback_and_timeout_and_arguments_0(
                callback.as_ref().unchecked_ref(),
                delay,
            )
            .map_err(|_| refused())?;
        Ok(Self {
            controller,
            scope,
            timer,
            _callback: callback,
            until,
        })
    }

    fn check(&self) -> io::Result<()> {
        if now() >= self.until || self.controller.signal().aborted() {
            return Err(refused());
        }
        Ok(())
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.scope.clear_timeout_with_handle(self.timer);
        self.controller.abort();
    }
}

pub struct S3Body(Option<Body>);

struct Body {
    abort: Deadline,
    reader: ReadableStreamDefaultReader,
    chunk: Option<Uint8Array>,
    remaining: u64,
}

impl ReadbackBody for S3Body {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        // Drop aborts a pending read and fences subsequent use after failure.
        let mut body = self.0.take().ok_or_else(refused)?;
        let count = body.read(dst).await?;
        self.0 = Some(body);
        Ok(count)
    }
}

impl Body {
    async fn read(&mut self, dst: &mut [u8]) -> io::Result<usize> {
        self.abort.check()?;
        if dst.is_empty() {
            return Ok(0);
        }
        if self.chunk.is_none() {
            let item = JsFuture::from(self.reader.read())
                .await
                .map_err(|_| refused())?;
            self.abort.check()?;
            let field =
                |name: &str| Reflect::get(&item, &JsValue::from_str(name)).map_err(|_| refused());
            if field("done")?.as_bool().ok_or_else(refused)? {
                return if self.remaining == 0 {
                    Ok(0)
                } else {
                    Err(refused())
                };
            }
            let chunk: Uint8Array = field("value")?.dyn_into().map_err(|_| refused())?;
            if chunk.length() == 0 || u64::from(chunk.length()) > self.remaining {
                return Err(refused());
            }
            self.chunk = Some(chunk);
        }
        let chunk = self.chunk.take().ok_or_else(refused)?;
        let count = (dst.len().min(chunk.length() as usize)) as u32;
        chunk.subarray(0, count).copy_to(&mut dst[..count as usize]);
        self.remaining -= u64::from(count);
        if count < chunk.length() {
            self.chunk = Some(chunk.subarray(count, chunk.length()));
        }
        Ok(count as usize)
    }
}

#[cfg(feature = "s3-transport-fixture")]
#[path = "../../tests/fixtures/s3_transport_wasm.rs"]
mod fixture;
