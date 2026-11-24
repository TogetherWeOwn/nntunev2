//! Version-pinned reads for one authorized encrypted response.

use std::{future::Future, ops::Range, time::SystemTime};

use nntune_core::{digest_page, digest_readback::ReadbackBody, media::HEADER_SIZE};

use super::{EnrolledPlayback, OpenPart, PlaybackError, S3RangeResponse, S3RangeResponseError};
use crate::s3_range_response::{read_exact, require_eof, validate_s3_range_metadata};

pub struct S3Get<'a> {
    pub configuration: [u8; 16],
    pub bucket: &'a str,
    pub key: &'a str,
    pub version: &'a str,
    /// Half-open ciphertext offsets. `None` selects the entire sidecar.
    pub range: Option<Range<u64>>,
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

/// Select the independently configured, authenticated reader for the exact
/// configuration and bucket, sign the supplied key/version/range, and disable
/// redirects and automatic decoding. Enforce the deadline for both GET and
/// pending body reads, and cancel them when dropped. Never retry after delivery.
pub trait S3PlaybackTransport {
    type Body: ReadbackBody;

    fn get(
        &mut self,
        get: S3Get<'_>,
    ) -> impl Future<Output = std::io::Result<S3GetReply<Self::Body>>>;
}

impl From<S3RangeResponseError> for PlaybackError {
    fn from(error: S3RangeResponseError) -> Self {
        Self::Response(error)
    }
}

pub struct S3EnrolledPlayback<'a, T: S3PlaybackTransport> {
    play: EnrolledPlayback<'a>,
    transport: T,
    media_body: Option<T::Body>,
    page: Option<(u32, Vec<u8>)>,
}

impl<'a> EnrolledPlayback<'a> {
    /// Attach the trusted transport to this response's existing plan and cursor.
    pub fn with_s3<T: S3PlaybackTransport>(self, transport: T) -> S3EnrolledPlayback<'a, T> {
        S3EnrolledPlayback {
            play: self,
            transport,
            media_body: None,
            page: None,
        }
    }
}

impl<T: S3PlaybackTransport> S3EnrolledPlayback<'_, T> {
    /// Open each sealed frame before returning its plaintext. There is only
    /// one media GET for the response and one sidecar GET per covered page.
    pub async fn next(
        &mut self,
        mut now: impl FnMut() -> SystemTime,
    ) -> Result<Option<OpenPart>, PlaybackError> {
        let Some(chunk) = self.play.next_chunk(now())? else {
            return Ok(None);
        };
        // Taking both before awaiting fences this response on failure or cancellation.
        let reader = self.play.reader.take().ok_or(PlaybackError::Stopped)?;
        let media_body = self.media_body.take();
        let index = chunk.selection.index;
        let page = chunk.selection.page;
        let until = self.play.pins.usable_until;
        let mut live = || now() < until;
        if self.page.as_ref().is_none_or(|(loaded, _)| *loaded != page) {
            self.page = Some((page, self.read_page(page, &mut live).await?));
        }
        let start = if index == 0 {
            0
        } else {
            chunk.ciphertext.start
        };
        let mut body = match media_body {
            Some(body) => body,
            None => self.open_media(start, &mut live).await?,
        };
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
        let (_, page) = self.page.as_ref().ok_or(PlaybackError::Binding)?;
        let part = self
            .play
            .open_part(reader, index, page, sealed, header.as_deref(), now)?;
        self.media_body = (!last).then_some(body);
        Ok(Some(part))
    }

    async fn read_page(
        &mut self,
        index: u32,
        live: &mut impl FnMut() -> bool,
    ) -> Result<Vec<u8>, PlaybackError> {
        let pins = &self.play.pins;
        let versions = pins.page_versions;
        let version = versions.get(index as usize).ok_or(PlaybackError::Binding)?;
        let key = digest_page::object_key(pins.object_id, pins.generation_id, pins.map_id, index);
        let geometry = self.play.geometry;
        let page = geometry.page(index).ok_or(PlaybackError::Binding)?;
        let len = page.encoded_len;
        let mut reply = self.get(&key, version, None, live).await?;
        if reply.status != 200
            || reply.content_range.is_some()
            || reply.content_length != Some(u64::from(len))
        {
            return Err(PlaybackError::Binding);
        }
        let bytes = read_exact(&mut reply.body, len as usize, live).await?;
        require_eof(&mut reply.body, live).await?;
        Ok(bytes)
    }

    async fn open_media(
        &mut self,
        start: u64,
        live: &mut impl FnMut() -> bool,
    ) -> Result<T::Body, PlaybackError> {
        let plan = self.play.plan;
        let end = plan.ciphertext_span().ok_or(PlaybackError::Binding)?.end;
        let span = start..end;
        let locator = self.play.locator;
        let version = self.play.pins.media_version;
        let reply = self
            .get(locator.key(), version, Some(span.clone()), live)
            .await?;
        validate_s3_range_metadata(
            plan.sealed_len(),
            span,
            locator.binding(),
            S3RangeResponse {
                status: reply.status,
                content_range: reply.content_range.as_deref(),
                content_length: reply.content_length,
                binding: Some(locator.binding()), // Checked against the observed version by get.
            },
        )?;
        Ok(reply.body)
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

    async fn get(
        &mut self,
        key: &str,
        version: &str,
        range: Option<Range<u64>>,
        live: &mut impl FnMut() -> bool,
    ) -> Result<S3GetReply<T::Body>, PlaybackError> {
        if !live() {
            return Err(PlaybackError::Expired);
        }
        if !(1..=1024).contains(&version.len())
            || version == "null"
            || !version.bytes().all(|b| b.is_ascii_graphic() && b != b',')
        {
            return Err(PlaybackError::Binding);
        }
        let reply = self
            .transport
            .get(S3Get {
                configuration: self.play.pins.provider_configuration,
                bucket: self.play.locator.bucket(),
                key,
                version,
                range,
                usable_until: self.play.pins.usable_until,
            })
            .await;
        if !live() {
            return Err(PlaybackError::Expired);
        }
        let reply = reply.map_err(|_| S3RangeResponseError::Body)?;
        if reply.version.as_deref() != Some(version) || reply.has_content_encoding {
            return Err(PlaybackError::Binding);
        }
        Ok(reply)
    }
}
