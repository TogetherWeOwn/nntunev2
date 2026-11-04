//! Bounded receiving account/ingest request I/O. Request cancellation is not
//! physical-close evidence: the account owner retains cleanup and capacity.

use std::future::Future;

use crate::account_pool::with_session_deadline;
use crate::account_route::RegistryError;

#[cfg(target_arch = "wasm32")]
pub(crate) mod worker;

pub(crate) struct Head<'a> {
    pub content_type: Option<&'a str>,
    pub content_length: Option<&'a str>,
    pub content_encoding: Option<&'a str>,
}

impl Head<'_> {
    pub fn validate(&self, cap: usize) -> Result<Option<usize>, RegistryError> {
        if self.content_type != Some("application/json") || self.content_encoding.is_some() {
            return Err(RegistryError);
        }
        self.content_length
            .map(|raw| {
                if raw.is_empty() || raw.len() > 10 || !raw.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(RegistryError);
                }
                raw.parse::<usize>()
                    .ok()
                    .filter(|n| *n <= cap)
                    .ok_or(RegistryError)
            })
            .transpose()
    }
}

pub(crate) trait Body {
    /// The adapter must check the raw chunk length before copying into Rust.
    async fn next(&mut self, remaining: usize) -> Result<Option<Vec<u8>>, RegistryError>;
}

pub(crate) async fn collect(
    mut body: impl Body,
    cap: usize,
    declared: Option<usize>,
) -> Result<Vec<u8>, RegistryError> {
    if declared.is_some_and(|n| n > cap) {
        return Err(RegistryError);
    }
    let limit = declared.unwrap_or(cap);
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next(limit - bytes.len()).await? {
        if chunk.is_empty() || chunk.len() > limit - bytes.len() {
            return Err(RegistryError);
        }
        bytes.extend_from_slice(&chunk);
    }
    if declared.is_some_and(|n| n != bytes.len()) {
        return Err(RegistryError);
    }
    Ok(bytes)
}

/// One request-level race spans body collection and dispatch. The losing work
/// future is dropped; retained account controls remain the owner's authority.
pub(crate) async fn run<F: Future>(
    work: F,
    cancellation: impl Future<Output = ()>,
    deadline: impl Future<Output = ()>,
) -> Result<F::Output, RegistryError> {
    with_session_deadline(with_session_deadline(work, cancellation), deadline)
        .await
        .map_err(|_| RegistryError)?
        .map_err(|_| RegistryError)
}

#[cfg(test)]
mod tests;
