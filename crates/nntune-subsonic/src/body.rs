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

fn exceeds_cap(current: usize, incoming: usize, cap: usize) -> bool {
    current.checked_add(incoming).is_none_or(|len| len > cap)
}

#[cfg(test)]
mod tests {
    #[test]
    fn length_overflow_is_oversized() {
        assert!(super::exceeds_cap(usize::MAX, 1, usize::MAX));
        assert!(!super::exceeds_cap(4, 6, 10));
        assert!(super::exceeds_cap(4, 7, 10));
    }
}

/// Accumulates body chunks under a byte cap and a declared `Content-Length`.
pub struct FormBody {
    declared: Option<usize>,
    cap: usize,
    bytes: Vec<u8>,
    oversized: bool,
}

impl std::fmt::Debug for FormBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FormBody").finish_non_exhaustive()
    }
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
            oversized: false,
        }
    }

    /// A declared length over the cap is refused before any byte is buffered.
    pub fn check_declared(&self) -> Result<(), BodyFault> {
        match self.declared {
            Some(declared) if declared > self.cap => Err(BodyFault::Oversized),
            _ => Ok(()),
        }
    }

    /// Append one chunk. An oversized body remains rejected through `finish`.
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), BodyFault> {
        self.check_declared()?;
        if self.oversized || exceeds_cap(self.bytes.len(), chunk.len(), self.cap) {
            self.oversized = true;
            return Err(BodyFault::Oversized);
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// Close the stream. A clean end-of-stream shorter than the declared
    /// length is a client abort the runtime reports as a normal end, so it is
    /// unreadable, never a smaller valid body.
    pub fn finish(self) -> Result<String, BodyFault> {
        self.check_declared()?;
        if self.oversized {
            return Err(BodyFault::Oversized);
        }
        if self
            .declared
            .is_some_and(|declared| self.bytes.len() < declared)
        {
            return Err(BodyFault::Unreadable);
        }
        String::from_utf8(self.bytes).map_err(|_| BodyFault::Unreadable)
    }
}
