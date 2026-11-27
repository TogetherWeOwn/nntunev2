//! R2 source selection and shared byte-prefix settlement.

#[cfg(any(test, target_arch = "wasm32"))]
use std::future::Future;

#[cfg(any(test, target_arch = "wasm32"))]
use nntune_core::transcode::artifact_r2_key;

#[cfg(any(test, target_arch = "wasm32"))]
use crate::{api_error::ApiError, grant, play_route};

#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) struct Play {
    pub(crate) user_id: String,
    pub(crate) play_id: String,
    /// `None` selects the legacy pointer; only `v3` selects an artifact.
    artifact_key: Option<String>,
}

#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) fn authorize(
    token: &str,
    track_id: &str,
    secret: &[u8],
    now_unix: u64,
) -> Result<Play, ApiError> {
    let (grant, artifact) =
        grant::verify_for_r2(token, secret, now_unix).map_err(|_| ApiError::unauthorized())?;
    let play_id = grant.play_id.ok_or_else(ApiError::unauthorized)?;
    if grant.track_id != track_id {
        return Err(ApiError::forbidden());
    }
    let artifact_key = artifact
        .as_deref()
        .map(artifact_r2_key)
        .transpose()
        .map_err(|_| ApiError::unauthorized())?;
    Ok(Play {
        user_id: grant.user_id,
        play_id,
        artifact_key,
    })
}

#[cfg(any(test, target_arch = "wasm32"))]
impl Play {
    pub(crate) fn content_type(&self) -> &'static str {
        self.artifact_key
            .as_ref()
            .map_or(play_route::PLAY_CONTENT_TYPE, |_| "audio/ogg")
    }

    pub(crate) async fn blob(
        &self,
        legacy_key: impl Future<Output = Result<String, ApiError>>,
        head: impl AsyncFnOnce(&str) -> Option<u64>,
    ) -> Result<Blob, ApiError> {
        let key = match &self.artifact_key {
            Some(key) => key.clone(),
            None => legacy_key.await?,
        };
        let size = head(&key)
            .await
            .ok_or_else(ApiError::upstream_unavailable)?;
        Ok(Blob { key, size })
    }
}

/// Keep the selected key private so HEAD and the body read cannot diverge.
#[cfg(any(test, target_arch = "wasm32"))]
pub(crate) struct Blob {
    key: String,
    pub(crate) size: u64,
}

#[cfg(any(test, target_arch = "wasm32"))]
impl Blob {
    pub(crate) async fn read<T>(
        self,
        get: impl AsyncFnOnce(String) -> Option<T>,
    ) -> Result<T, ApiError> {
        get(self.key)
            .await
            .ok_or_else(ApiError::upstream_unavailable)
    }
}

/// Report only the pulled prefix on drop, including a cancelled read.
pub(crate) struct Delivery<F: FnOnce(u64)> {
    written: u64,
    remaining: Option<u64>,
    report: Option<F>,
}

impl<F: FnOnce(u64)> Delivery<F> {
    pub(crate) fn new(expected: u64, report: F) -> Self {
        Self {
            written: 0,
            remaining: Some(expected),
            report: Some(report),
        }
    }

    pub(crate) fn record(&mut self, len: usize) {
        let len = len as u64;
        self.remaining = self.remaining.and_then(|left| left.checked_sub(len));
        if self.remaining.is_some() {
            self.written += len;
        }
    }
}

impl<F: FnOnce(u64)> Drop for Delivery<F> {
    fn drop(&mut self) {
        let written = self.written;
        if let Some(report) = self.report.take().filter(|_| written > 0) {
            report(written);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, FutureExt, StreamExt, TryStreamExt};
    use std::{cell::Cell, collections::BTreeMap};

    const HASH: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
    const SECRET: &[u8] = b"test only, not a deployed secret";
    // Independently generated HMAC-SHA256 wire vector for this fixture.
    const TOKEN: &str = concat!("v3.dXNlci0xfHRyYWNrLTl8cGxheS03fDQyfDIwMHw5ZDcwZjExYmM3NTlkNzM0",
        "ZWJiMzQzZjU5MzJmMDA1Y2ViYjM0M2Y1OTMyZjAwNWM5ZDcwZjExYmM3NTlkNzM0.RscNSYHWhiP5Byo3P0kFFPdrWczsJIjsb8kBReUuDRk");
    const BROWSER_TOKEN: &str =
        include_str!("../../../fixtures/play-grant/browser-rendition.token");

    #[test]
    fn signed_rendition_is_served_without_legacy_fallback() {
        let track_id = "22222222-2222-2222-2222-222222222222";
        let browser = authorize(BROWSER_TOKEN, track_id, SECRET, 199).unwrap();
        assert_eq!(browser.artifact_key, Some(format!("audio/9d/70/{HASH}")));
        assert!(matches!(authorize(TOKEN, "track-8", SECRET, 199), Err(err) if err.status == 403));
        assert!(matches!(authorize(TOKEN, "track-9", SECRET, 200), Err(err) if err.status == 401));
        assert!(grant::verify(TOKEN, SECRET, 199).is_err());
        let rendition_key = format!("audio/9d/70/{HASH}");
        let legacy_hash = "a".repeat(64);
        let legacy_key = format!("audio/{legacy_hash}");
        let mut objects: BTreeMap<&str, &[u8]> = BTreeMap::from([
            (rendition_key.as_str(), b"rendition".as_slice()),
            ("track/track-9", legacy_hash.as_bytes()),
            (legacy_key.as_str(), b"legacy".as_slice()),
        ]);
        let pointer_reads = Cell::new(0);
        let fetch = |objects: &BTreeMap<&str, &[u8]>, token: &str| {
            async {
                let legacy = async {
                    pointer_reads.set(pointer_reads.get() + 1);
                    Ok(crate::cache::blob_key_for_pointer(objects["track/track-9"]).unwrap())
                };
                let play = authorize(token, "track-9", SECRET, 199)?;
                let blob = play
                    .blob(legacy, async |key| Some(objects.get(key)?.len() as u64))
                    .await?;
                let size = blob.size;
                blob.read(async |key| Some(objects.get(key.as_str())?.to_vec()))
                    .await
                    .map(|bytes| (play.content_type(), size, bytes))
            }
            .now_or_never()
            .unwrap()
        };
        let legacy = grant::issue_v2("user-1", "track-9", "play-7", 42, 200, SECRET);
        assert_eq!(
            fetch(&objects, &legacy).unwrap(),
            ("audio/flac", 6, b"legacy".to_vec())
        );
        pointer_reads.set(0);
        assert_eq!(
            fetch(&objects, TOKEN).unwrap(),
            ("audio/ogg", 9, b"rendition".to_vec())
        );
        objects.remove(rendition_key.as_str()).unwrap();
        let error = fetch(&objects, TOKEN).unwrap_err();
        assert_eq!((error.status, error.code), (502, "upstream_unavailable"));
        assert_eq!(pointer_reads.get(), 0);
    }

    #[test]
    fn rendition_range_meters_only_a_pulled_prefix_when_dropped() {
        let play = authorize(TOKEN, "track-9", SECRET, 199).unwrap();
        assert_eq!(play.user_id, "user-1");
        assert_eq!(play.play_id, "play-7");
        let delivered = Cell::new(None);
        let report = |written| delivered.set(Some(written));
        drop(Delivery::new(6, report));
        assert_eq!(delivered.get(), None);
        let mut delivery = Delivery::new(6, report);
        let mut body = stream::iter([Ok::<_, ()>(b"234"), Ok(b"567")])
            .inspect_ok(move |chunk| delivery.record(chunk.len()));
        body.next().now_or_never().unwrap().unwrap().unwrap();
        drop(body);
        assert_eq!(delivered.get(), Some(3));
    }

    #[test]
    fn overrun_preserves_only_the_prior_delivery_prefix() {
        let delivered = Cell::new(None);
        let mut delivery = Delivery::new(6, |written| delivered.set(Some(written)));
        delivery.record(3);
        delivery.record(4);
        delivery.record(2);
        drop(delivery);
        assert_eq!(delivered.get(), Some(3));
    }
}
