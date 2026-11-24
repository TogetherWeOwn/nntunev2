//! Private playback for the stream route, with an injected committed source.

use std::{
    future::{poll_fn, Future},
    io,
    pin::pin,
    task::Poll,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nntune_core::{
    digest_geometry::DigestGeometry,
    digest_read_snapshot::DigestReadSnapshot,
    digest_readback::{ReadbackBody, ReadbackPin},
    original_source::SourceError,
};

use super::{EnrolledMedia, EnrolledPlayback, S3Get, S3GetReply};
use crate::{
    api_error::ApiError,
    delivered::DeliveredInterval,
    grant::{self, Grant},
    original_play::OriginalWriter,
    play_route::PlayRouteRequest,
    r2_play::Delivery,
    range::{self, RangeOutcome},
};

#[cfg(target_arch = "wasm32")]
pub(crate) mod worker;

pub struct CatalogueMedia<'a> {
    /// The reference from the member's catalogue, independent of the snapshot.
    pub reference: [u8; 16],
    pub pins: DigestReadSnapshot<'a>,
    pub media: EnrolledMedia<'a>,
}

/// Resolve the exact signed member, track, play and epoch. Confirm the
/// protected read commit and independently resolve the manifest, envelope
/// and configured bucket. GET operations use read-only credentials and the
/// exact version, with strict HTTP framing, no redirects or decoding, and
/// cancellation on drop or at the supplied deadline.
#[allow(async_fn_in_trait)] // Isolate-local I/O.
pub trait PrivateSource {
    type Body: ReadbackBody;

    async fn resolve(&self, grant: &Grant) -> Option<CatalogueMedia<'_>>;
    async fn lease_live(&self, grant: &Grant) -> bool;
    async fn media(&self, request: S3Get<'_>) -> io::Result<S3GetReply<Self::Body>>;
    /// Return the observed version and exactly `length` bytes from a 200 with
    /// unambiguous Content-Length, no Content-Range and no Content-Encoding.
    async fn page(
        &self,
        bucket: &str,
        pin: ReadbackPin<'_>,
        length: u32,
        until: SystemTime,
    ) -> io::Result<(String, Vec<u8>)>;
}

pub struct ResponseRange {
    pub status: u16,
    pub len: u64,
    pub content_range: Option<String>,
}

/// Render errors before `start` at the HTTP boundary. For a 416, the start
/// callback renders the refusal and returns no writer. The caller cancels
/// this future on expiry or disconnect and its writer aborts on drop.
/// Only the sink-acknowledged prefix is reported, including on drop.
pub async fn serve<S: PrivateSource, W: OriginalWriter>(
    request: &PlayRouteRequest,
    secret: &[u8],
    source: Option<&S>,
    now: impl Fn() -> SystemTime,
    start: impl FnOnce(ResponseRange) -> Result<Option<W>, ApiError>,
    report: impl FnOnce(DeliveredInterval),
) -> Result<(), ApiError> {
    let unavailable = ApiError::upstream_unavailable;
    let now_unix = now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ApiError::unauthorized())?;
    let (grant, digest) = grant::verify_private(
        request.token.as_deref().unwrap_or(""),
        secret,
        now_unix.as_secs(),
    )
    .map_err(|_| ApiError::unauthorized())?;
    if grant.track_id != request.track_id {
        return Err(ApiError::forbidden());
    }
    let source = source.ok_or_else(ApiError::not_found)?;
    let CatalogueMedia {
        reference,
        mut pins,
        media,
    } = source
        .resolve(&grant)
        .await
        .ok_or_else(ApiError::not_found)?;
    let cutoff = UNIX_EPOCH
        .checked_add(Duration::from_secs(grant.exp_unix))
        .ok_or_else(ApiError::unauthorized)?;
    pins.usable_until = pins.usable_until.min(cutoff);
    let until = pins.usable_until;
    let len = pins.plaintext_len;
    let resolved_digest: String = pins
        .plaintext_sha256
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if reference != pins.reference_id
        || digest != resolved_digest
        || len == 0
        || now() >= until
        || !source.lease_live(&grant).await
    {
        return Err(ApiError::not_found());
    }
    let locator = media.media.locator();
    let page_versions = pins.page_versions;
    let configuration = pins.provider_configuration;
    let map = u128::from_be_bytes(pins.map_id);
    let (outcome, play) = EnrolledPlayback::start(request.range.as_deref(), pins, media, now())
        .map_err(|_| unavailable())?;
    let (offset, size, status, content_range) = match outcome {
        RangeOutcome::Full => (0, len, 200, None),
        RangeOutcome::Partial(slice) => (
            slice.start,
            slice.len(),
            206,
            Some(range::content_range_value(slice, len)),
        ),
        RangeOutcome::Unsatisfiable => {
            start(ResponseRange {
                status: 416,
                len: 0,
                content_range: Some(format!("bytes */{len}")),
            })?;
            return Ok(());
        }
    };
    let mut play = play
        .ok_or_else(unavailable)?
        .open_s3(&now, async |get| source.media(get).await)
        .await
        .map_err(|_| unavailable())?;
    let geometry = DigestGeometry::new(len).ok_or_else(unavailable)?;
    let object = locator
        .key()
        .strip_prefix("media/")
        .ok_or_else(unavailable)?;
    let mut start = Some(|| {
        start(ResponseRange {
            status,
            len: size,
            content_range,
        })
    });
    let mut writer: Option<W> = None;
    let mut delivery = Delivery::new(size, |served| {
        report(DeliveredInterval {
            user_id: grant.user_id.clone(),
            play_id: grant.play_id.clone().expect("v4 has a play id"),
            track_id: grant.track_id.clone(),
            start: offset,
            end: offset + served,
            file_bytes: Some(len),
        })
    });
    let result = async {
        let mut cached = None;
        while let Some(index) = play.next_page(now()).map_err(|_| unavailable())? {
            let next = async {
                if cached
                    .as_ref()
                    .is_none_or(|(loaded, _, _)| *loaded != index)
                {
                    let version = &page_versions[index as usize];
                    if matches!(version.as_str(), "" | "null")
                        || version.len() > 1024
                        || !version
                            .bytes()
                            .all(|byte| byte.is_ascii_graphic() && byte != b',')
                    {
                        return Err(unavailable());
                    }
                    let key = format!("media-digests/{object}/{map:032x}/{index:08x}");
                    let size = geometry.page(index).ok_or_else(unavailable)?.encoded_len;
                    let pin = ReadbackPin {
                        configuration,
                        object_key: &key,
                        version: Some(version),
                    };
                    let (observed, bytes) = source
                        .page(locator.bucket(), pin, size, until)
                        .await
                        .map_err(|_| unavailable())?;
                    if &observed != version || bytes.len() != size as usize {
                        return Err(unavailable());
                    }
                    cached = Some((index, observed, bytes));
                }
                let (_, version, page) = cached.as_ref().expect("selected page");
                let part = play
                    .next((version, page), &now)
                    .await
                    .map_err(|_| unavailable())?
                    .ok_or_else(unavailable)?;
                if !source.lease_live(&grant).await {
                    return Err(ApiError::not_found());
                }
                Ok(part)
            };
            let part = {
                let mut next = pin!(next);
                poll_fn(|cx| {
                    // A cancelled response must wake the pump even while a
                    // read is its only outstanding operation.
                    if writer
                        .as_mut()
                        .is_some_and(|w| w.poll_cancel(cx).is_ready())
                    {
                        return Poll::Ready(Err(unavailable()));
                    }
                    next.as_mut().poll(cx)
                })
                .await?
            };
            if let Some(start) = start.take() {
                writer = start()?;
            }
            let writer = writer.as_mut().ok_or_else(ApiError::internal_error)?;
            let mut remaining = part.as_slice();
            while !remaining.is_empty() {
                let accepted = poll_fn(|cx| {
                    if now() >= until || writer.poll_cancel(cx).is_ready() {
                        return Poll::Ready(Err(SourceError::Cancelled));
                    }
                    writer.poll_write(cx, remaining)
                })
                .await
                .map_err(|_| unavailable())?;
                if accepted == 0 {
                    return Err(unavailable());
                }
                play.written(accepted).map_err(|_| unavailable())?;
                delivery.record(accepted);
                remaining = &remaining[accepted..];
            }
        }
        play.finish().map_err(|_| unavailable())?;
        let writer = writer.as_mut().ok_or_else(ApiError::internal_error)?;
        poll_fn(|cx| writer.poll_close(cx))
            .await
            .map_err(|_| unavailable())
    }
    .await;
    if let Some(writer) = writer.as_mut().filter(|_| result.is_err()) {
        writer.abort();
    }
    result
}
