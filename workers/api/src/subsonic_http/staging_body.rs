//! Inert staging form collection and Worker stream adapter.
//!
//! The adapter reads JS chunks directly: `worker` 0.8.7's `ByteStream`
//! copies a whole chunk before exposing its length. The 8 KiB cap limits
//! logical input, not platform-owned chunks, allocator capacity/overhead,
//! linear memory, or the later construction graph. No mount is enabled.

use nntune_subsonic::{BodyFault, FormBody};

use super::{validate_staging_sources, CredentialParseError, STAGING_SOURCE_BYTES};

/// Payload-free collection failures. A failure permanently rejects the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingBodyError {
    TooLarge,
    Unreadable,
    Parameters(CredentialParseError),
}

/// Incremental sink for already-borrowed chunks whose producer does not make
/// an unbounded application allocation before calling `push`.
pub struct StagingBody {
    body: FormBody,
    fault: Option<StagingBodyError>,
    received: usize,
}

impl StagingBody {
    /// Reject an oversized declared length before reading any chunks.
    pub fn new(declared: Option<usize>) -> Result<Self, StagingBodyError> {
        let body = FormBody::with_cap(declared, STAGING_SOURCE_BYTES);
        body.check_declared().map_err(map_fault)?;
        Ok(Self {
            body,
            fault: None,
            received: 0,
        })
    }

    /// Check actual bytes before growing the existing form buffer. A stream
    /// error is opaque and poisons the sink, so partial input cannot succeed.
    pub fn push(&mut self, chunk: Result<&[u8], ()>) -> Result<(), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let result = match chunk {
            Ok(bytes) => self.body.push(bytes).map_err(map_fault).map(|()| {
                self.received += bytes.len();
            }),
            Err(()) => Err(StagingBodyError::Unreadable),
        };
        if let Err(fault) = result {
            self.fault = Some(fault);
        }
        result
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn push_external(
        &mut self,
        len: usize,
        copy: impl FnOnce(&mut [u8]),
    ) -> Result<(), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self
            .received
            .checked_add(len)
            .is_none_or(|n| n > STAGING_SOURCE_BYTES)
        {
            self.fault = Some(StagingBodyError::TooLarge);
            return Err(StagingBodyError::TooLarge);
        }
        // This bounded temporary and the sink buffer coexist during the copy;
        // their allocator capacities are not an isolate-memory bound.
        let mut bytes = vec![0; len];
        copy(&mut bytes);
        self.push(Ok(&bytes))
    }

    /// Read a Worker request without using `ByteStream`'s unbounded JS-to-Rust
    /// conversion. A failed or locked stream is never dispatched. The caller
    /// owns request cancellation/deadlines; releasing the reader is not proof
    /// that platform buffers have been reclaimed.
    #[cfg(target_arch = "wasm32")]
    pub async fn collect_worker(
        mut self,
        request: &worker::Request,
        query: Option<&str>,
    ) -> Result<String, StagingBodyError> {
        use worker::wasm_bindgen::{JsCast, JsValue};
        use worker::{js_sys, wasm_bindgen_futures::JsFuture, web_sys};

        let request = request.inner();
        if request.body_used() {
            return Err(StagingBodyError::Unreadable);
        }
        let Some(stream) = request.body() else {
            return self.finish(query);
        };
        let reader = web_sys::ReadableStreamDefaultReader::new(&stream)
            .map_err(|_| StagingBodyError::Unreadable)?;
        let result = async {
            loop {
                let item = JsFuture::from(reader.read())
                    .await
                    .map_err(|_| StagingBodyError::Unreadable)?;
                let done = js_sys::Reflect::get(&item, &JsValue::from_str("done"))
                    .map_err(|_| StagingBodyError::Unreadable)?
                    .as_bool()
                    .ok_or(StagingBodyError::Unreadable)?;
                if done {
                    break Ok(());
                }
                let value = js_sys::Reflect::get(&item, &JsValue::from_str("value"))
                    .map_err(|_| StagingBodyError::Unreadable)?;
                let chunk = value
                    .dyn_into::<js_sys::Uint8Array>()
                    .map_err(|_| StagingBodyError::Unreadable)?;
                self.push_external(chunk.length() as usize, |out| chunk.copy_to(out))?;
            }
        }
        .await;
        reader.release_lock();
        result?;
        self.finish(query)
    }

    /// Finish only after clean EOF; validate both sources before decoding.
    pub fn finish(self, query: Option<&str>) -> Result<String, StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let body = self.body.finish().map_err(map_fault)?;
        validate_staging_sources(query, Some(body.as_bytes()))
            .map_err(StagingBodyError::Parameters)?;
        Ok(body)
    }
}

// This export exists only in the synthetic runtime test build, never in the
// ordinary Worker bundle. It exercises the same entry point used by callers.
#[cfg(all(target_arch = "wasm32", feature = "staging-body-wasm-test"))]
mod wasm_test {
    use super::StagingBody;
    use worker::wasm_bindgen::{self, prelude::*, JsValue};

    #[wasm_bindgen]
    pub async fn collect_staging_test(
        request: worker::web_sys::Request,
        declared: Option<u32>,
    ) -> Result<String, JsValue> {
        let body = StagingBody::new(declared.map(|n| n as usize))
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        body.collect_worker(&request.into(), None)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))
    }
}

fn map_fault(fault: BodyFault) -> StagingBodyError {
    match fault {
        BodyFault::Oversized => StagingBodyError::TooLarge,
        BodyFault::Unreadable => StagingBodyError::Unreadable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_length_is_checked_before_chunks() {
        assert!(matches!(
            StagingBody::new(Some(STAGING_SOURCE_BYTES + 1)),
            Err(StagingBodyError::TooLarge)
        ));
        assert!(StagingBody::new(Some(STAGING_SOURCE_BYTES)).is_ok());
    }

    #[test]
    fn external_chunks_are_rejected_before_copy() {
        let mut body = StagingBody::new(Some(0)).unwrap();
        body.push_external(2, |out| out.copy_from_slice(b"x="))
            .unwrap();
        body.push_external(STAGING_SOURCE_BYTES - 2, |out| out.fill(b'a'))
            .unwrap();
        assert_eq!(body.finish(None).unwrap().len(), STAGING_SOURCE_BYTES);

        for first in [0, STAGING_SOURCE_BYTES - 1] {
            let mut body = StagingBody::new(None).unwrap();
            body.push_external(first, |out| out.fill(b'a')).unwrap();
            let mut copied = false;
            assert_eq!(
                body.push_external(STAGING_SOURCE_BYTES - first + 1, |_| copied = true),
                Err(StagingBodyError::TooLarge)
            );
            assert!(!copied);
            assert_eq!(
                body.push_external(0, |_| copied = true),
                Err(StagingBodyError::TooLarge)
            );
            assert!(!copied);
            assert_eq!(body.finish(None), Err(StagingBodyError::TooLarge));
        }
    }

    #[test]
    fn actual_bytes_are_bounded_across_chunks_even_with_dishonest_length() {
        for declared in [None, Some(0), Some(1), Some(STAGING_SOURCE_BYTES)] {
            let mut body = StagingBody::new(declared).unwrap();
            let a = vec![b'a'; STAGING_SOURCE_BYTES / 2 - 1];
            body.push(Ok(b"x=")).unwrap();
            body.push(Ok(&a)).unwrap();
            body.push(Ok(&a)).unwrap();
            assert_eq!(body.finish(None).unwrap().len(), STAGING_SOURCE_BYTES);

            let mut body = StagingBody::new(declared).unwrap();
            body.push(Ok(b"x=")).unwrap();
            body.push(Ok(&a)).unwrap();
            body.push(Ok(&a)).unwrap();
            assert_eq!(body.push(Ok(b"x")), Err(StagingBodyError::TooLarge));
            assert_eq!(body.finish(None), Err(StagingBodyError::TooLarge));
        }
    }

    #[test]
    fn transport_failure_short_body_and_preflight_fail_closed() {
        let mut body = StagingBody::new(Some(3)).unwrap();
        body.push(Ok(b"ab")).unwrap();
        assert_eq!(body.finish(None), Err(StagingBodyError::Unreadable));
        let mut body = StagingBody::new(None).unwrap();
        body.push(Ok(b"u=a")).unwrap();
        assert_eq!(body.push(Err(())), Err(StagingBodyError::Unreadable));
        assert_eq!(body.push(Ok(b"&u=b")), Err(StagingBodyError::Unreadable));
        assert_eq!(body.finish(None), Err(StagingBodyError::Unreadable));
        let mut body = StagingBody::new(None).unwrap();
        body.push(Ok(b"x=1")).unwrap();
        let query = "x=1&".repeat(super::super::STAGING_PARAMS_PER_SOURCE + 1);
        assert_eq!(
            body.finish(Some(&query)),
            Err(StagingBodyError::Parameters(
                CredentialParseError::TooManyParams
            ))
        );
    }
}
