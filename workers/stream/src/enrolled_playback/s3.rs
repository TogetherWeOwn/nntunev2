//! Version-pinned ciphertext for one authorized playback response.

use std::{ops::Range, time::SystemTime};

use nntune_core::{
    digest_readback::ReadbackBody,
    media::HEADER_SIZE,
    s3_locator::{S3Binding, S3Locator},
};

use super::{EnrolledPlayback, OpenPart, PlaybackError, S3RangeResponse, S3RangeResponseError};
use crate::s3_range_response::{read_exact, require_eof, validate_s3_range_metadata};

pub struct S3Get<'a> {
    pub configuration: [u8; 16],
    pub locator: &'a S3Locator,
    /// Half-open ciphertext offsets into the pinned media object.
    pub range: Range<u64>,
    pub usable_until: SystemTime,
}

pub struct S3GetReply<B> {
    pub status: u16,
    /// Observed, unambiguous headers. Any ambiguous metadata is a transport error.
    pub version: Option<String>,
    pub content_range: Option<String>,
    pub content_length: Option<u64>,
    pub has_content_encoding: bool,
    pub body: B,
}

impl From<S3RangeResponseError> for PlaybackError {
    fn from(error: S3RangeResponseError) -> Self {
        Self::Response(error)
    }
}

pub struct S3EnrolledPlayback<'a, B> {
    play: EnrolledPlayback<'a>,
    body: Option<B>,
}

impl<'a> EnrolledPlayback<'a> {
    /// Open the response's one media GET after the caller authorizes the snapshot.
    /// The opener selects the independently configured reader, signs the exact
    /// bucket, key, version and span, and disables redirects and decoding.
    /// Enforce the request deadline and cancellation for GET and body reads.
    pub async fn open_s3<B: ReadbackBody>(
        mut self,
        mut now: impl FnMut() -> SystemTime,
        get: impl AsyncFnOnce(S3Get<'_>) -> std::io::Result<S3GetReply<B>>,
    ) -> Result<S3EnrolledPlayback<'a, B>, PlaybackError> {
        let first = self.next_chunk(now())?.ok_or(PlaybackError::Stopped)?;
        let end = self
            .plan
            .ciphertext_span()
            .ok_or(PlaybackError::Binding)?
            .end;
        let start = if first.selection.index == 0 {
            0
        } else {
            first.ciphertext.start
        };
        let range = start..end;
        let locator = self.locator;
        let version = self.pins.media_version;
        if version.contains(',') {
            return Err(PlaybackError::Binding);
        }
        let reply = get(S3Get {
            configuration: self.pins.provider_configuration,
            locator,
            range: range.clone(),
            usable_until: self.pins.usable_until,
        })
        .await;
        if now() >= self.pins.usable_until {
            return Err(PlaybackError::Expired);
        }
        let reply = reply.map_err(|_| S3RangeResponseError::Body)?;
        if reply.has_content_encoding {
            return Err(PlaybackError::Binding);
        }
        let binding = reply.version.map(S3Binding::VersionId);
        validate_s3_range_metadata(
            self.plan.sealed_len(),
            range,
            locator.binding(),
            S3RangeResponse {
                status: reply.status,
                content_range: reply.content_range.as_deref(),
                content_length: reply.content_length,
                binding: binding.as_ref(),
            },
        )?;
        Ok(S3EnrolledPlayback {
            play: self,
            body: Some(reply.body),
        })
    }
}

impl<B: ReadbackBody> S3EnrolledPlayback<'_, B> {
    /// Select the next committed page before the caller opens its pinned sidecar.
    pub fn next_page(&mut self, now: SystemTime) -> Result<Option<u32>, PlaybackError> {
        Ok(self.play.next_chunk(now)?.map(|chunk| chunk.selection.page))
    }

    /// Open one complete frame before releasing its plaintext. The caller
    /// binds sidecar I/O to the canonical key, version, length and deadline.
    pub async fn next(
        &mut self,
        page: (&str, &[u8]),
        mut now: impl FnMut() -> SystemTime,
    ) -> Result<Option<OpenPart>, PlaybackError> {
        let Some(chunk) = self.play.next_chunk(now())? else {
            return Ok(None);
        };
        // Take both before awaiting so failure or cancellation fences the response.
        let reader = self.play.reader.take().ok_or(PlaybackError::Stopped)?;
        let mut body = self.body.take().ok_or(PlaybackError::Stopped)?;
        let index = chunk.selection.index;
        if !self.play.matches_page(chunk.selection.page, page.0) {
            return Err(PlaybackError::Binding);
        }
        let until = self.play.pins.usable_until;
        let mut live = || now() < until;
        let header = if index == 0 {
            Some(read_exact(&mut body, HEADER_SIZE as usize, &mut live).await?)
        } else {
            None
        };
        let len = (chunk.ciphertext.end - chunk.ciphertext.start) as usize;
        let sealed = read_exact(&mut body, len, &mut live).await?;
        let last = self.play.plan.chunks().is_some_and(|(_, end)| index == end);
        if last {
            require_eof(&mut body, &mut live).await?;
        }
        let part = self
            .play
            .open_part(reader, index, page.1, sealed, header.as_deref(), now)?;
        self.body = (!last).then_some(body);
        Ok(Some(part))
    }

    pub fn written(&mut self, bytes: usize) -> Result<(), PlaybackError> {
        self.play.written(bytes)
    }

    pub fn served_bytes(&self) -> u64 {
        self.play.served_bytes()
    }

    pub fn finish(self) -> Result<(), PlaybackError> {
        self.play.finish()
    }
}
