//! Inert staging form collection; this is not a Worker stream adapter.
//!
//! `worker` 0.8.7's `ByteStream` converts each entire JS chunk to a Rust
//! Vec before exposing its length. It cannot safely feed this collector:
//! reject a present Worker body without polling that stream until an adapter
//! can check the JS chunk length before copying. Platform-owned chunks and
//! parsing, allocator overhead/capacity, and the later construction graph are
//! outside the logical byte cap here. No mount is enabled by this module.

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
}

impl StagingBody {
    /// Reject an oversized declared length before reading any chunks.
    pub fn new(declared: Option<usize>) -> Result<Self, StagingBodyError> {
        let body = FormBody::with_cap(declared, STAGING_SOURCE_BYTES);
        body.check_declared().map_err(map_fault)?;
        Ok(Self { body, fault: None })
    }

    /// Check actual bytes before growing the existing form buffer. A stream
    /// error is opaque and poisons the sink, so partial input cannot succeed.
    pub fn push(&mut self, chunk: Result<&[u8], ()>) -> Result<(), StagingBodyError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        let result = match chunk {
            Ok(bytes) => self.body.push(bytes).map_err(map_fault),
            Err(()) => Err(StagingBodyError::Unreadable),
        };
        if let Err(fault) = result {
            self.fault = Some(fault);
        }
        result
    }

    /// A Worker body stream must fail closed: its adapter allocates before
    /// this sink can inspect the chunk. Do not poll it or trust its length.
    pub fn unsupported_worker_stream(self) -> StagingBodyError {
        StagingBodyError::Unreadable
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
        assert_eq!(
            StagingBody::new(None).unwrap().unsupported_worker_stream(),
            StagingBodyError::Unreadable
        );
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
