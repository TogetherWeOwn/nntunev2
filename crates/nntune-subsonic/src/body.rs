//! Transport-free collector for a POST form body.
//!
//! The Worker shell feeds it chunks from the runtime stream; every rule that
//! decides whether the body may be dispatched lives here so native tests can
//! drive it with fake streams. Any fault means the request must be answered
//! with [`crate::unreadable_body_response`] or [`crate::oversized_body_response`]
//! and must not reach the dispatcher with partial parameters.

use crate::app::MAX_BODY_BYTES;

/// Why a form body must not be dispatched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BodyFault {
    /// The body is, or would become, larger than the cap.
    Oversized,
    /// The stream failed, ended short of its declared length, or was not UTF-8.
    Unreadable,
}

/// Accumulates body chunks under a byte cap and a declared `Content-Length`.
#[derive(Debug)]
pub struct FormBody {
    declared: Option<usize>,
    cap: usize,
    bytes: Vec<u8>,
}

impl FormBody {
    /// `declared` is the parsed `Content-Length`, if the client sent one.
    pub fn new(declared: Option<usize>) -> FormBody {
        FormBody::with_cap(declared, MAX_BODY_BYTES)
    }

    /// Same as [`FormBody::new`] with an explicit cap (tests use a small one).
    pub fn with_cap(declared: Option<usize>, cap: usize) -> FormBody {
        FormBody {
            declared,
            cap,
            bytes: Vec::new(),
        }
    }

    /// A declared length over the cap is refused before any byte is buffered.
    pub fn check_declared(&self) -> Result<(), BodyFault> {
        match self.declared {
            Some(declared) if declared > self.cap => Err(BodyFault::Oversized),
            _ => Ok(()),
        }
    }

    /// Append one chunk, refusing as soon as the cap would be crossed.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), BodyFault> {
        if self.bytes.len().saturating_add(chunk.len()) > self.cap {
            return Err(BodyFault::Oversized);
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// Close the stream. A clean end-of-stream shorter than the declared
    /// length is a client abort the runtime reports as a normal end, so it is
    /// unreadable, never a smaller valid body.
    pub fn finish(self) -> Result<String, BodyFault> {
        if self
            .declared
            .is_some_and(|declared| self.bytes.len() < declared)
        {
            return Err(BodyFault::Unreadable);
        }
        String::from_utf8(self.bytes).map_err(|_| BodyFault::Unreadable)
    }
}
