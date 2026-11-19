//! Inert staging form collection and Worker stream adapter.
//!
//! The adapter reads JS chunks directly: `worker` 0.9.0's `ByteStream`
//! copies a whole chunk before exposing its length. One charged 8 KiB Rust
//! buffer owns the form bytes. Platform-owned chunks and allocator residency
//! remain outside this charge. No mount is enabled.

use nntune_subsonic::construction_budget::ConstructionBudget;

use super::{validate_staging_sources, CredentialParseError, STAGING_SOURCE_BYTES};

/// Payload-free collection failures. A failure permanently rejects the body.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingBodyError {
    TooLarge,
    Unreadable,
    Capacity,
    Parameters(CredentialParseError),
}

/// Incremental sink. The caller transfers an existing request ledger so raw
/// form collection and later construction cannot each start with a fresh cap.
pub struct StagingBody {
    declared: Option<usize>,
    bytes: Vec<u8>,
    budget: ConstructionBudget,
    fault: Option<StagingBodyError>,
}

impl StagingBody {
    /// Reject an oversized declared length before reading any chunks.
    pub fn new(
        declared: Option<usize>,
        budget: ConstructionBudget,
    ) -> Result<Self, StagingBodyError> {
        if declared.is_some_and(|len| len > STAGING_SOURCE_BYTES) {
            return Err(StagingBodyError::TooLarge);
        }
        Ok(Self {
            declared,
            budget,
            bytes: Vec::new(),
            fault: None,
        })
    }

    /// Check actual bytes before copying into the form buffer. A stream
    /// error is opaque and poisons the sink, so partial input cannot succeed.
    pub fn push(&mut self, chunk: Result<&[u8], ()>) -> Result<(), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        match chunk {
            Ok(bytes) => self.push_external(bytes.len(), |out| out.copy_from_slice(bytes)),
            Err(()) => self.fail(StagingBodyError::Unreadable),
        }
    }

    fn fail(&mut self, fault: StagingBodyError) -> Result<(), StagingBodyError> {
        self.fault = Some(fault);
        Err(fault)
    }

    fn push_external(
        &mut self,
        len: usize,
        copy: impl FnOnce(&mut [u8]),
    ) -> Result<(), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let start = self.bytes.len();
        let Some(end) = start
            .checked_add(len)
            .filter(|&end| end <= STAGING_SOURCE_BYTES)
        else {
            return self.fail(StagingBodyError::TooLarge);
        };
        if len != 0 && self.bytes.capacity() == 0 {
            self.bytes = match self.budget.allocate_exact(STAGING_SOURCE_BYTES) {
                Ok(bytes) => bytes,
                Err(_) => return self.fail(StagingBodyError::Capacity),
            };
        }
        self.bytes.resize(end, 0);
        copy(&mut self.bytes[start..end]);
        Ok(())
    }

    /// Read a Worker request without using `ByteStream`'s unbounded JS-to-Rust
    /// conversion. A failed or locked stream is never dispatched. The caller
    /// owns request cancellation/deadlines; releasing the reader is not proof
    /// that platform buffers have been reclaimed.
    #[cfg(target_arch = "wasm32")]
    pub async fn collect_worker(
        mut self,
        request: &worker::web_sys::Request,
        query: Option<&str>,
    ) -> Result<(String, ConstructionBudget), StagingBodyError> {
        use worker::wasm_bindgen::{JsCast, JsValue};
        use worker::{js_sys, wasm_bindgen_futures::JsFuture, web_sys};

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

    /// Finish only after clean EOF; transfer the allocation and its ledger.
    pub fn finish(
        self,
        query: Option<&str>,
    ) -> Result<(String, ConstructionBudget), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.declared.is_some_and(|len| self.bytes.len() < len) {
            return Err(StagingBodyError::Unreadable);
        }
        let body = String::from_utf8(self.bytes).map_err(|_| StagingBodyError::Unreadable)?;
        validate_staging_sources(query, Some(body.as_bytes()))
            .map_err(StagingBodyError::Parameters)?;
        Ok((body, self.budget))
    }
}

// This export exists only in the synthetic runtime test build, never in the
// ordinary Worker bundle. It exercises the same entry point used by callers.
#[cfg(all(target_arch = "wasm32", feature = "staging-body-wasm-test"))]
mod wasm_test {
    use super::{ConstructionBudget, StagingBody, STAGING_SOURCE_BYTES};
    use worker::wasm_bindgen::{self, prelude::*, JsValue};

    #[wasm_bindgen]
    pub async fn collect_staging_test(
        request: worker::web_sys::Request,
        declared: Option<u32>,
        one_short: Option<bool>,
    ) -> Result<String, JsValue> {
        let mut limit = ConstructionBudget::new(usize::MAX);
        limit.charge(STAGING_SOURCE_BYTES, 1, 1).unwrap();
        let budget =
            ConstructionBudget::new(limit.charged() - usize::from(one_short.unwrap_or(false)));
        let sink = StagingBody::new(declared.map(|n| n as usize), budget)
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        let (body, budget) = sink
            .collect_worker(&request, None)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:?}")))?;
        if body.is_empty() {
            assert_eq!((body.capacity(), budget.charged()), (0, 0));
        } else {
            assert_eq!(body.capacity(), STAGING_SOURCE_BYTES);
            assert_eq!(budget.charged(), limit.charged());
        }
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector(declared: Option<usize>) -> Result<StagingBody, StagingBodyError> {
        StagingBody::new(declared, ConstructionBudget::new(usize::MAX))
    }

    #[test]
    fn body_capacity_is_charged_before_copy_and_carried_through_eof() {
        let mut expected = ConstructionBudget::new(usize::MAX);
        expected.charge(STAGING_SOURCE_BYTES, 1, 1).unwrap();
        let limit = expected.charged();
        let mut short = StagingBody::new(None, ConstructionBudget::new(limit - 1)).unwrap();
        assert_eq!(
            short.push_external(1, |_| panic!("copied without a charge")),
            Err(StagingBodyError::Capacity)
        );
        assert_eq!(short.finish(None).err(), Some(StagingBodyError::Capacity));

        let mut exact = StagingBody::new(None, ConstructionBudget::new(limit)).unwrap();
        exact.push(Ok(b"x=")).unwrap();
        exact.push_external(1, |out| out[0] = b'a').unwrap();
        let (body, ledger) = exact.finish(None).unwrap();
        assert_eq!(body, "x=a");
        assert_eq!(body.capacity(), STAGING_SOURCE_BYTES);
        assert_eq!(ledger.charged(), limit);

        let empty = StagingBody::new(None, ConstructionBudget::new(0)).unwrap();
        let (body, ledger) = empty.finish(None).unwrap();
        assert_eq!((body.capacity(), ledger.charged()), (0, 0));
    }

    #[test]
    fn declared_length_is_checked_before_chunks() {
        assert!(matches!(
            collector(Some(STAGING_SOURCE_BYTES + 1)),
            Err(StagingBodyError::TooLarge)
        ));
        assert!(collector(Some(STAGING_SOURCE_BYTES)).is_ok());
    }

    #[test]
    fn external_chunks_are_rejected_before_copy() {
        let mut body = collector(Some(0)).unwrap();
        body.push_external(2, |out| out.copy_from_slice(b"x="))
            .unwrap();
        body.push_external(STAGING_SOURCE_BYTES - 2, |out| out.fill(b'a'))
            .unwrap();
        assert_eq!(body.finish(None).unwrap().0.len(), STAGING_SOURCE_BYTES);

        for first in [0, STAGING_SOURCE_BYTES - 1] {
            let mut body = collector(None).unwrap();
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
            assert_eq!(body.finish(None).err(), Some(StagingBodyError::TooLarge));
        }
    }

    #[test]
    fn actual_bytes_are_bounded_across_chunks_even_with_dishonest_length() {
        for declared in [None, Some(0), Some(1), Some(STAGING_SOURCE_BYTES)] {
            let mut body = collector(declared).unwrap();
            let a = vec![b'a'; STAGING_SOURCE_BYTES / 2 - 1];
            body.push(Ok(b"x=")).unwrap();
            body.push(Ok(&a)).unwrap();
            body.push(Ok(&a)).unwrap();
            assert_eq!(body.finish(None).unwrap().0.len(), STAGING_SOURCE_BYTES);

            let mut body = collector(declared).unwrap();
            body.push(Ok(b"x=")).unwrap();
            body.push(Ok(&a)).unwrap();
            body.push(Ok(&a)).unwrap();
            assert_eq!(body.push(Ok(b"x")), Err(StagingBodyError::TooLarge));
            assert_eq!(body.finish(None).err(), Some(StagingBodyError::TooLarge));
        }
    }

    #[test]
    fn transport_failure_short_body_and_preflight_fail_closed() {
        let mut body = collector(Some(3)).unwrap();
        body.push(Ok(b"ab")).unwrap();
        assert_eq!(body.finish(None).err(), Some(StagingBodyError::Unreadable));
        let mut body = collector(None).unwrap();
        body.push(Ok(b"u=a")).unwrap();
        assert_eq!(body.push(Err(())), Err(StagingBodyError::Unreadable));
        assert_eq!(body.push(Ok(b"&u=b")), Err(StagingBodyError::Unreadable));
        assert_eq!(body.finish(None).err(), Some(StagingBodyError::Unreadable));
        let mut body = collector(None).unwrap();
        body.push(Ok(b"x=1")).unwrap();
        let query = "x=1&".repeat(super::super::STAGING_PARAMS_PER_SOURCE + 1);
        assert_eq!(
            body.finish(Some(&query)).err(),
            Some(StagingBodyError::Parameters(
                CredentialParseError::TooManyParams
            ))
        );
    }
}
