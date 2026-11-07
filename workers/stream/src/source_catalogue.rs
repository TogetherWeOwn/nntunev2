//! Grant-bound internal original-source catalogue, off-route until runtime
//! qualification. No source encryption keys, public fetch or response driver.

#[cfg(any(test, target_arch = "wasm32"))]
mod transport;
#[cfg(target_arch = "wasm32")]
pub use transport::worker::WorkerSourceTransport;

use std::cell::Cell;

use nntune_core::{
    original_source::{
        canonical_track_id,
        service::{Lookup, Payload, MAX_LOOKUP_MS, PATH},
        OriginalSource,
    },
    pool_channel::Channel,
};

use crate::{
    account_playback::{CatalogueUnavailable, OriginalCatalogue},
    grant::{self, Grant},
};

/// Exactly one POST to PATH on the API service binding, no public URL/redirect,
/// retries or caching. Enforce the absolute deadline across fetch and body read;
/// bound the body to MAX_RECORD_BYTES and the selected-job header to 64 bytes
/// while reading. Only a no-store successful response yields a Payload. Drop
/// cancels I/O, with no detached read task. Never log auth, headers or bytes.
#[allow(async_fn_in_trait)]
pub trait SourceTransport {
    async fn fetch(
        &self,
        authorization: &str,
        body: &[u8],
        deadline_ms: u64,
    ) -> Result<Payload, CatalogueUnavailable>;
}

pub struct SourceCatalogue<'a, T, F> {
    transport: &'a T,
    token: &'a str,
    track_id: &'a str,
    play_grant_key: &'a [u8],
    now_ms: F,
    last_ms: Cell<u64>,
}

impl<'a, T: SourceTransport, F: Fn() -> u64> SourceCatalogue<'a, T, F> {
    /// Construction verifies authority but performs no I/O. Runtime must check
    /// its hard qualification gate even before constructing this adapter.
    pub fn new(
        transport: &'a T,
        token: &'a str,
        track_id: &'a str,
        play_grant_key: &'a [u8],
        now_ms: F,
    ) -> Result<Self, CatalogueUnavailable> {
        let now = now_ms();
        let out = Self {
            transport,
            token,
            track_id,
            play_grant_key,
            now_ms,
            last_ms: Cell::new(now),
        };
        out.authorize()?;
        Ok(out)
    }

    fn authorize(&self) -> Result<(Grant, u64), CatalogueUnavailable> {
        let now = (self.now_ms)();
        let last = self.last_ms.get();
        self.last_ms.set(last.max(now));
        if now < last || self.play_grant_key.len() < 32 || !canonical_track_id(self.track_id) {
            return Err(CatalogueUnavailable);
        }
        let grant = grant::verify(self.token, self.play_grant_key, now / 1000)
            .map_err(|_| CatalogueUnavailable)?;
        if grant.play_id.is_none() || grant.track_id != self.track_id {
            return Err(CatalogueUnavailable);
        }
        Ok((grant, now))
    }
}

impl<T: SourceTransport, F: Fn() -> u64> OriginalCatalogue for SourceCatalogue<'_, T, F> {
    async fn source(&self, track_id: &str) -> Result<Option<OriginalSource>, CatalogueUnavailable> {
        let (grant, now) = self.authorize()?;
        if track_id != self.track_id {
            return Err(CatalogueUnavailable);
        }
        let lookup = Lookup {
            track_id: self.track_id.into(),
            grant_expires_at: grant.exp_unix,
            issued_at_ms: now,
            deadline_ms: now
                .saturating_add(MAX_LOOKUP_MS)
                .min(grant.exp_unix.saturating_mul(1000)),
        };
        if !lookup.valid_at(now) {
            return Err(CatalogueUnavailable);
        }
        let body = serde_json::to_vec(&lookup).map_err(|_| CatalogueUnavailable)?;
        let authorization = Channel::OriginalSource
            .authorization_header(Some(self.play_grant_key), PATH, now / 1000, &body)
            .ok_or(CatalogueUnavailable)?;
        let payload = self
            .transport
            .fetch(&authorization, &body, lookup.deadline_ms)
            .await?;
        let (_, now) = self.authorize()?;
        if !lookup.valid_at(now) {
            return Err(CatalogueUnavailable);
        }
        let source = payload
            .parse(self.track_id)
            .map_err(|_| CatalogueUnavailable)?;
        let (_, now) = self.authorize()?;
        if !lookup.valid_at(now) {
            return Err(CatalogueUnavailable);
        }
        Ok(Some(source))
    }
}

#[cfg(test)]
mod tests;
