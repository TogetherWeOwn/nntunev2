//! Worker-only HTTP adapter. Supplying a Secret does not install or verify a
//! runtime binding; no route or schedule constructs this client yet.

#![cfg(any(target_arch = "wasm32", test))]

use super::{ValidationError, MAX_RESPONSE_BYTES};
use crate::videolinks::{is_valid_video_id, videos_list_query, VIDEOS_LIST_MAX_IDS};

const ENDPOINT: &str = "https://www.googleapis.com/youtube/v3/videos";
const FIELDS: &str =
    "items(id,status(embeddable,privacyStatus),contentDetails(duration),snippet(title,channelId))";

/// Deliberately has no Debug/Display implementation: the key and URL must not
/// enter logs. The caller obtains the Secret from the approved runtime binding.
#[cfg(target_arch = "wasm32")]
pub struct WorkerValidationClient {
    key: String,
}

#[cfg(target_arch = "wasm32")]
impl WorkerValidationClient {
    pub fn from_secret(secret: worker::Secret) -> Result<Self, ValidationError> {
        let key = secret.to_string();
        validate_key(&key)?;
        Ok(Self { key })
    }
}

fn validate_key(key: &str) -> Result<(), ValidationError> {
    if key.is_empty()
        || key.len() > 256
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(ValidationError::Transport);
    }
    Ok(())
}

fn request_url(ids: &[String], key: &str) -> Result<worker::Url, ValidationError> {
    if ids.is_empty()
        || ids.len() > VIDEOS_LIST_MAX_IDS
        || ids.iter().any(|id| !is_valid_video_id(id))
        || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
    {
        return Err(ValidationError::InvalidQueue);
    }
    validate_key(key)?;
    let mut url = worker::Url::parse(ENDPOINT).map_err(|_| ValidationError::Transport)?;
    url.set_query(Some(&videos_list_query(ids)));
    url.query_pairs_mut()
        .append_pair("fields", FIELDS)
        .append_pair("key", key);
    Ok(url)
}

/// Check the chunk length before copying any JS bytes into Wasm memory. The
/// fixed-capacity buffer never grows past the response budget, even without a
/// Content-Length header or when the server sends one oversized chunk.
struct BoundedBody(Vec<u8>);

impl BoundedBody {
    fn new() -> Self {
        Self(Vec::with_capacity(MAX_RESPONSE_BYTES))
    }

    fn chunk(&mut self, len: usize) -> Result<&mut [u8], ValidationError> {
        if len > MAX_RESPONSE_BYTES - self.0.len() {
            return Err(ValidationError::InvalidResponse);
        }
        let start = self.0.len();
        self.0.resize(start + len, 0);
        Ok(&mut self.0[start..])
    }

    fn finish(self) -> Result<String, ValidationError> {
        String::from_utf8(self.0).map_err(|_| ValidationError::InvalidResponse)
    }
}

struct AbortOnDrop<F: FnOnce()>(Option<F>);

impl<F: FnOnce()> Drop for AbortOnDrop<F> {
    fn drop(&mut self) {
        if let Some(abort) = self.0.take() {
            abort();
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl super::ValidationClient for WorkerValidationClient {
    async fn videos_list(
        &self,
        ids: &[String],
    ) -> Result<super::ValidationResponse, ValidationError> {
        use worker::wasm_bindgen::JsCast;
        use worker::{Fetch, Headers, Method, Request, RequestInit, RequestRedirect};

        let url = request_url(ids, &self.key)?;
        // This deadline includes both headers and all response chunks. A dropped
        // JsFuture alone does not cancel fetch, so keep the abort guard alive
        // until body consumption completes (including every early error).
        super::with_deadline(std::time::Duration::from_secs(10), async {
            let headers = Headers::new();
            headers
                .set("accept", "application/json")
                .map_err(|_| ValidationError::Transport)?;
            headers
                .set("cache-control", "no-store")
                .map_err(|_| ValidationError::Transport)?;
            let mut init = RequestInit::new();
            init.with_method(Method::Get)
                .with_headers(headers)
                .with_redirect(RequestRedirect::Error);
            let request = Request::new_with_init(url.as_str(), &init)
                .map_err(|_| ValidationError::Transport)?;
            let controller = worker::AbortController::default();
            let signal = controller.signal();
            let _abort = AbortOnDrop(Some(move || controller.abort()));
            let response = Fetch::Request(request)
                .send_with_signal(&signal)
                .await
                .map_err(|_| ValidationError::Transport)?;
            let status = response.status_code();
            if status != 200 {
                // Do not read API error bodies, redirects or quota diagnostics.
                return Err(ValidationError::HttpStatus(status));
            }
            if let Some(length) = response
                .headers()
                .get("content-length")
                .map_err(|_| ValidationError::Transport)?
            {
                if length
                    .parse::<usize>()
                    .map_err(|_| ValidationError::InvalidResponse)?
                    > MAX_RESPONSE_BYTES
                {
                    return Err(ValidationError::InvalidResponse);
                }
            }
            // Use the JS reader rather than ByteStream: ByteStream copies each
            // entire chunk into a Vec before callers can check its length.
            let raw: worker::web_sys::Response = response.into();
            let stream = raw.body().ok_or(ValidationError::InvalidResponse)?;
            let reader = stream
                .get_reader()
                .dyn_into::<worker::web_sys::ReadableStreamDefaultReader>()
                .map_err(|_| ValidationError::Transport)?;
            let _release = AbortOnDrop(Some(|| reader.release_lock()));
            let mut body = BoundedBody::new();
            loop {
                let result = worker::wasm_bindgen_futures::JsFuture::from(reader.read())
                    .await
                    .map_err(|_| ValidationError::Transport)?;
                let done = worker::js_sys::Reflect::get(&result, &"done".into())
                    .map_err(|_| ValidationError::Transport)?
                    .as_bool()
                    .ok_or(ValidationError::InvalidResponse)?;
                if done {
                    break;
                }
                let chunk = worker::js_sys::Reflect::get(&result, &"value".into())
                    .map_err(|_| ValidationError::Transport)?
                    .dyn_into::<worker::js_sys::Uint8Array>()
                    .map_err(|_| ValidationError::InvalidResponse)?;
                chunk.copy_to(body.chunk(chunk.length() as usize)?);
            }
            Ok(super::ValidationResponse {
                status,
                body: body.finish()?,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_request_has_only_validation_parts_and_no_thumbnail_fields() {
        let ids = vec!["v0000000000".into(), "v0000000001".into()];
        let url = request_url(&ids, "synthetic_key-not-a-credential").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("www.googleapis.com"));
        assert_eq!(url.path(), "/youtube/v3/videos");
        assert!(url.fragment().is_none());
        assert!(url.username().is_empty());
        let pairs: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(pairs.len(), 5);
        assert_eq!(pairs["part"], "status,contentDetails,snippet");
        assert_eq!(pairs["id"], ids.join(","));
        assert_eq!(pairs["maxResults"], "50");
        assert_eq!(pairs["fields"], FIELDS);
        assert_eq!(pairs["key"], "synthetic_key-not-a-credential");
        assert!(!FIELDS.contains("thumbnail"));
    }

    #[test]
    fn invalid_ids_and_credentials_fail_before_request_construction() {
        for ids in [
            Vec::new(),
            vec!["bad".into()],
            vec!["v0000000000".into(); 2],
            vec!["v0000000000&key=injected".into()],
            (0..51).map(|i| format!("v{i:010}")).collect(),
        ] {
            assert_eq!(
                request_url(&ids, "synthetic").err(),
                Some(ValidationError::InvalidQueue)
            );
        }
        for key in [
            "",
            "has space",
            "line\nbreak",
            "injected&part=other",
            &"x".repeat(257),
        ] {
            let err = request_url(&["v0000000000".into()], key).unwrap_err();
            assert_eq!(err, ValidationError::Transport);
            assert!(!format!("{err:?}").contains(key) || key.is_empty());
        }
        let ids: Vec<_> = (0..50).map(|i| format!("v{i:010}")).collect();
        assert!(request_url(&ids, "synthetic").is_ok());
    }

    #[test]
    fn body_accepts_exact_byte_budget_and_rejects_before_copying_excess() {
        let mut body = BoundedBody::new();
        body.chunk(3).unwrap().copy_from_slice(b"abc");
        body.chunk(MAX_RESPONSE_BYTES - 3).unwrap().fill(b' ');
        assert_eq!(body.0.len(), MAX_RESPONSE_BYTES);
        assert_eq!(body.chunk(1), Err(ValidationError::InvalidResponse));
        assert_eq!(body.0.len(), MAX_RESPONSE_BYTES);
        assert_eq!(body.finish().unwrap().len(), MAX_RESPONSE_BYTES);
        let mut body = BoundedBody::new();
        assert_eq!(
            body.chunk(MAX_RESPONSE_BYTES + 1),
            Err(ValidationError::InvalidResponse)
        );
        assert!(body.0.is_empty());
        assert_eq!(
            body.chunk(usize::MAX),
            Err(ValidationError::InvalidResponse)
        );
    }

    #[test]
    fn body_decodes_utf8_only_after_all_chunks_and_sanitizes_errors() {
        let mut body = BoundedBody::new();
        for byte in "é".as_bytes() {
            body.chunk(1).unwrap()[0] = *byte;
        }
        assert_eq!(body.finish().unwrap(), "é");
        let mut body = BoundedBody::new();
        body.chunk(1).unwrap()[0] = 0xff;
        assert_eq!(body.finish(), Err(ValidationError::InvalidResponse));
    }

    #[test]
    fn cancellation_guard_runs_on_return_and_caller_drop() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};
        let aborted = std::cell::Cell::new(0);
        {
            let _guard = AbortOnDrop(Some(|| aborted.set(aborted.get() + 1)));
        }
        assert_eq!(aborted.get(), 1);
        let mut future = Box::pin(async {
            let _guard = AbortOnDrop(Some(|| aborted.set(aborted.get() + 1)));
            std::future::pending::<()>().await;
        });
        assert_eq!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        );
        drop(future);
        assert_eq!(aborted.get(), 2);
    }
}
