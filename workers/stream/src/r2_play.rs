//! Source selection and byte accounting shared by the R2 fetch and its fixtures.

use futures_util::{Stream, TryStreamExt};
use nntune_core::transcode::artifact_r2_key;

use crate::{api_error::ApiError, grant, play_route};

pub(crate) struct Play {
    pub(crate) user_id: String,
    pub(crate) play_id: String,
    /// `None` selects the legacy pointer; only `v3` selects an artifact.
    pub(crate) artifact_key: Option<String>,
}

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

impl Play {
    pub(crate) fn content_type(&self) -> &'static str {
        self.artifact_key
            .as_ref()
            .map_or(play_route::PLAY_CONTENT_TYPE, |_| "audio/ogg")
    }
}

/// Report the pulled prefix when the response body is dropped, even if a
/// later chunk was never read (or errored).
pub(crate) fn meter<S, F, E>(inner: S, expected: u64, report: F) -> impl Stream<Item = S::Item>
where
    S: Stream<Item = Result<Vec<u8>, E>>,
    F: FnOnce(u64),
{
    let mut delivery = Delivery {
        written: 0,
        expected,
        report: Some(report),
    };
    inner.inspect_ok(move |chunk| delivery.record(chunk.len()))
}

struct Delivery<F: FnOnce(u64)> {
    written: u64,
    expected: u64,
    report: Option<F>,
}

impl<F: FnOnce(u64)> Delivery<F> {
    fn record(&mut self, len: usize) {
        self.written = self.written.saturating_add(len as u64);
    }
}

impl<F: FnOnce(u64)> Drop for Delivery<F> {
    fn drop(&mut self) {
        let written = self.written.min(self.expected);
        if let Some(report) = self.report.take().filter(|_| written > 0) {
            report(written);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::{self, RangeOutcome};
    use futures_util::{stream, task::noop_waker_ref};
    use std::cell::Cell;

    const HASH: &str = "9d70f11bc759d734ebb343f5932f005cebb343f5932f005c9d70f11bc759d734";
    const SECRET: &[u8] = b"test only, not a deployed secret";
    // Independently generated HMAC-SHA256 wire vector for this fixture.
    const TOKEN: &str = concat!("v3.dXNlci0xfHRyYWNrLTl8cGxheS03fDQyfDIwMHw5ZDcwZjExYmM3NTlkNzM0",
        "ZWJiMzQzZjU5MzJmMDA1Y2ViYjM0M2Y1OTMyZjAwNWM5ZDcwZjExYmM3NTlkNzM0.RscNSYHWhiP5Byo3P0kFFPdrWczsJIjsb8kBReUuDRk");

    #[test]
    fn r2_rendition_selects_only_the_signed_track_and_object() {
        let play = authorize(TOKEN, "track-9", SECRET, 199).unwrap();
        assert_eq!((&*play.user_id, &*play.play_id), ("user-1", "play-7"));
        assert_eq!(play.content_type(), "audio/ogg");
        assert_eq!(play.artifact_key, Some(format!("audio/9d/70/{HASH}")));
        assert!(matches!(authorize(TOKEN, "track-8", SECRET, 199), Err(err) if err.status == 403));
        assert!(matches!(authorize(TOKEN, "track-9", SECRET, 200), Err(err) if err.status == 401));
        assert!(grant::verify(TOKEN, SECRET, 199).is_err());

        let legacy = grant::issue_v2("user-1", "track-9", "play-7", 42, 200, SECRET);
        let legacy = authorize(&legacy, "track-9", SECRET, 199).unwrap();
        assert_eq!(legacy.content_type(), "audio/flac");
        assert_eq!(legacy.artifact_key, None);
    }

    #[test]
    fn rendition_range_meters_only_a_pulled_prefix_when_dropped() {
        let play = authorize(TOKEN, "track-9", SECRET, 199).unwrap();
        let bytes = b"0123456789";
        let size = bytes.len() as u64;
        assert_eq!(
            range::parse(Some("bytes=10-"), size),
            RangeOutcome::Unsatisfiable
        );
        let RangeOutcome::Partial(slice) = range::parse(Some("bytes=2-7"), size) else {
            panic!("expected seek range");
        };
        assert_eq!(range::content_range_value(slice, size), "bytes 2-7/10");
        let source = || stream::iter([Ok::<_, ()>(bytes[2..5].to_vec()), Ok(bytes[5..8].to_vec())]);
        let delivered = Cell::new(None);
        let report = |written| {
            delivered.set(Some((
                play.play_id.as_str(),
                slice.start,
                slice.start + written,
            )))
        };
        drop(meter(source(), slice.len(), report));
        assert_eq!(delivered.get(), None);
        let mut body = Box::pin(meter(source(), slice.len(), report));
        assert!(body
            .as_mut()
            .poll_next(&mut std::task::Context::from_waker(noop_waker_ref()))
            .is_ready());
        drop(body);
        assert_eq!(delivered.get(), Some(("play-7", 2, 5)));
    }
}
