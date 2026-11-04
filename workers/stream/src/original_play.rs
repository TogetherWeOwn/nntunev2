//! Off-route original-media response driver. This does not install a source
//! service, account supplier or public miss path. Authorize before resolving
//! the private source; the API remains its sole envelope/key/DB owner.
//!
//! Pulling verified media is not delivery. The response adapter acknowledges
//! only bytes accepted by its writer, then finishes or drops this response.
//! Drop settles a partial interval once, with no cache fill or further I/O.

use std::task::{Context, Poll};

use nntune_core::original_source::{
    reader::{AsyncArticleSource, AsyncRangeReader, AsyncReadControl, ReadControl},
    OriginalSource, SourceError,
};

use crate::{
    api_error::ApiError,
    delivered::{DeliveredInterval, DeliveredSink},
    grant::{self, Grant},
    play_route::{PlayRouteRefusal, PlayRouteRequest, PLAY_CONTENT_TYPE},
    range::{self, ByteRange, RangeOutcome},
};

/// A verified v2 authority, obtained before any private source lookup. The
/// source resolver and account supplier must recheck it after their awaits.
/// No public source wire protocol is defined by this in-process seam.
pub struct OriginalAuthorization {
    grant: Grant,
}

fn refusal(status: u16, api_error: ApiError) -> PlayRouteRefusal {
    PlayRouteRefusal {
        status,
        api_error,
        retry_after: None,
    }
}

pub fn authorize_original(
    request: &PlayRouteRequest,
    secret: &[u8],
    now_unix: u64,
) -> Result<OriginalAuthorization, PlayRouteRefusal> {
    if secret.is_empty() {
        return Err(refusal(500, ApiError::internal_error()));
    }
    let grant = grant::verify(request.token.as_deref().unwrap_or(""), secret, now_unix)
        .map_err(|_| refusal(401, ApiError::unauthorized()))?;
    if grant.play_id.is_none() || grant.track_id != request.track_id {
        return Err(refusal(401, ApiError::unauthorized()));
    }
    Ok(OriginalAuthorization { grant })
}

/// Request clock and wakeable cancellation/deadline authority. `poll_cancel`
/// must wake at the grant expiry as well as abort/operation deadlines. The
/// eventual account supplier still enforces its own <=20 s operation timer.
pub trait OriginalControl: AsyncReadControl {
    fn now_unix(&self) -> u64;
}

impl OriginalAuthorization {
    pub fn check<C: OriginalControl>(&self, control: &mut C) -> Result<(), SourceError> {
        control.check()?;
        if control.now_unix() >= self.grant.exp_unix {
            return Err(SourceError::Cancelled);
        }
        Ok(())
    }

    /// Prepare HTTP metadata from an already authenticated, track-bound
    /// source. Call `check` after source resolution and before preparing it.
    /// An unavailable source is not proof of an unknown catalogue track; this
    /// seam never derives a 404 from an article or source error.
    pub fn prepare<'a, C: OriginalControl>(
        self,
        source: &'a OriginalSource,
        range_header: Option<&str>,
        control: &mut C,
        sink: &'a mut dyn DeliveredSink,
    ) -> Result<OriginalPlay<'a>, PlayRouteRefusal> {
        self.check(control)
            .map_err(|_| refusal(401, ApiError::unauthorized()))?;
        if source.track_id() != self.grant.track_id {
            return Err(refusal(502, ApiError::upstream_unavailable()));
        }
        let size = source.file_size();
        let (status, selected) = match range::parse(range_header, size) {
            RangeOutcome::Full => (
                200,
                ByteRange {
                    start: 0,
                    end: size - 1,
                },
            ),
            RangeOutcome::Partial(selected) => (206, selected),
            RangeOutcome::Unsatisfiable => {
                return Err(refusal(416, ApiError::range_not_satisfiable()));
            }
        };
        let mut headers = vec![
            ("content-type".into(), PLAY_CONTENT_TYPE.into()),
            ("accept-ranges".into(), "bytes".into()),
            ("content-length".into(), selected.len().to_string()),
        ];
        if status == 206 {
            headers.push((
                "content-range".into(),
                range::content_range_value(selected, size),
            ));
        }
        let reader = AsyncRangeReader::new(source, selected.start..selected.end + 1)
            .map_err(|_| refusal(502, ApiError::upstream_unavailable()))?;
        Ok(OriginalPlay {
            status,
            headers,
            body_len: selected.len(),
            reader,
            authority: self,
            sink,
            start: selected.start,
            file_bytes: size,
            written: 0,
            offered: 0,
            stopped: false,
        })
    }
}

struct AuthorizedControl<'a, C> {
    authority: &'a OriginalAuthorization,
    control: &'a mut C,
}

impl<C: OriginalControl> ReadControl for AuthorizedControl<'_, C> {
    fn check(&mut self) -> Result<(), SourceError> {
        self.authority.check(self.control)
    }
}

impl<C: OriginalControl> AsyncReadControl for AuthorizedControl<'_, C> {
    fn poll_cancel(&mut self, cx: &mut Context<'_>) -> Poll<SourceError> {
        if let Err(error) = self.check() {
            return Poll::Ready(error);
        }
        self.control.poll_cancel(cx)
    }
}

/// HTTP metadata plus a lazy verified body. No runtime is installed yet:
/// actual Worker enqueue/abort behavior must be separately qualified.
pub struct OriginalPlay<'a> {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body_len: u64,
    reader: AsyncRangeReader<'a>,
    authority: OriginalAuthorization,
    sink: &'a mut dyn DeliveredSink,
    start: u64,
    file_bytes: u64,
    written: u64,
    offered: usize,
    stopped: bool,
}

impl OriginalPlay<'_> {
    /// Fetch at most the next output article slice. Before another pull, the
    /// writer must acknowledge the entire previous slice, or drop/abort the
    /// response after a partial write. Never count the fetched bytes here.
    pub async fn next<T: AsyncArticleSource, C: OriginalControl>(
        &mut self,
        transport: &mut T,
        control: &mut C,
    ) -> Result<Option<Vec<u8>>, SourceError> {
        if self.stopped || self.offered != 0 {
            self.stopped = true;
            return Err(SourceError::Cancelled);
        }
        self.stopped = true;
        let mut authorized = AuthorizedControl {
            authority: &self.authority,
            control,
        };
        let bytes = self.reader.next(transport, &mut authorized).await?;
        self.offered = bytes.as_ref().map_or(0, Vec::len);
        self.stopped = false;
        Ok(bytes)
    }

    /// Acknowledge only the ordered prefix actually accepted by the response
    /// writer. On writer failure, acknowledge its accepted prefix, then drop.
    /// This API cannot infer delivery from an account receipt or a pull.
    pub fn written(&mut self, bytes: usize) -> Result<(), SourceError> {
        if self.stopped || bytes > self.offered {
            self.stopped = true;
            return Err(SourceError::Bounds);
        }
        self.written += bytes as u64; // Bounded by the offered range slices.
        self.offered -= bytes;
        Ok(())
    }

    pub fn served_bytes(&self) -> u64 {
        self.written
    }

    /// No drain, cache publication or account release. Drop reports exactly
    /// the acknowledged bytes even when completion is refused.
    pub fn finish(self) -> Result<(), SourceError> {
        if self.stopped || self.offered != 0 || self.written != self.body_len {
            return Err(SourceError::Transport);
        }
        Ok(())
    }

    pub fn abort(self) {}
}

impl Drop for OriginalPlay<'_> {
    fn drop(&mut self) {
        if self.written == 0 {
            return;
        }
        self.sink.delivered(DeliveredInterval {
            user_id: self.authority.grant.user_id.clone(),
            play_id: self
                .authority
                .grant
                .play_id
                .clone()
                .expect("verified v2 grant"),
            track_id: self.authority.grant.track_id.clone(),
            start: self.start,
            end: self.start + self.written,
            file_bytes: Some(self.file_bytes),
        });
    }
}
