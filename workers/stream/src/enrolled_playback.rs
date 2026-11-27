//! Unmounted playback from an independently authorized, committed read snapshot.

use std::time::SystemTime;

use nntune_core::{
    digest_enrollment::PlaybackManifest,
    digest_geometry::{DigestGeometry, RangeGeometry, SealedRangeChunk},
    digest_range::{DigestRange, OpenPart, RangeError},
    digest_read_snapshot::DigestReadSnapshot,
    digest_readback::ReadbackBody,
    media_envelope::EnvelopeContext,
    s3_locator::{S3Binding, S3Locator, S3MediaMetadata},
};

use crate::{
    range::{plan_encrypted_range, RangeOutcome},
    response_delivery::OrderedDelivery,
    s3_range_response::{
        read_s3_range_chunk, valid_version, S3RangeResponse, S3RangeResponseError,
    },
};

pub mod route;
mod s3;
pub use s3::{S3EnrolledPlayback, S3Get, S3GetReply};
#[cfg(target_arch = "wasm32")]
mod read_observation;
#[cfg(target_arch = "wasm32")]
pub mod s3_transport;

/// Independently resolved custody for that same committed root and lifecycle.
pub struct EnrolledMedia<'a> {
    pub media: &'a S3MediaMetadata,
    pub manifest: PlaybackManifest<'a>,
    pub context: &'a EnvelopeContext,
    pub root: &'a [u8; 32],
    pub expected_record: &'a [u8; 230],
    pub retained_record: &'a [u8],
}

#[derive(Debug, PartialEq, Eq)]
pub enum PlaybackError {
    Binding,
    Expired,
    Stopped,
    Delivery,
    Response(S3RangeResponseError),
    Frame(RangeError),
}

pub struct EnrolledPlayback<'a> {
    reader: Option<DigestRange<'a>>,
    plan: RangeGeometry,
    locator: &'a S3Locator,
    pins: DigestReadSnapshot<'a>,
    delivery: OrderedDelivery,
}

impl<'a> EnrolledPlayback<'a> {
    /// The caller must match the committed snapshot's reference to the grant
    /// and resolve custody and the bucket from independently trusted metadata.
    pub fn start(
        range: Option<&str>,
        pins: DigestReadSnapshot<'a>,
        input: EnrolledMedia<'a>,
        now: SystemTime,
    ) -> Result<(RangeOutcome, Option<Self>), PlaybackError> {
        if now >= pins.usable_until {
            return Err(PlaybackError::Expired);
        }
        let len = input.media.plaintext_len();
        let geometry = DigestGeometry::new(len).ok_or(PlaybackError::Binding)?;
        let locator = input.media.locator();
        if input.context.identities
            != [
                pins.root_version_id,
                pins.environment_id,
                pins.library_id,
                pins.object_id,
                pins.generation_id,
                pins.provider_configuration,
            ]
            || pins.plaintext_len != len
            || !input.media.matches_context(input.context)
            || input.media.ciphertext_len() != geometry.sealed_len()
            || !matches!(locator.binding(), S3Binding::VersionId(v) if v == pins.media_version)
            || !valid_version(pins.media_version)
            || pins.page_versions.len() != geometry.pages() as usize
            || !pins
                .page_versions
                .iter()
                .all(|version| valid_version(version))
            || !input.manifest.matches(&pins, input.context)
        {
            return Err(PlaybackError::Binding);
        }
        let (outcome, plan) = plan_encrypted_range(range, geometry);
        let selection = match outcome {
            RangeOutcome::Full => 0..len,
            RangeOutcome::Partial(range) => {
                range.start..range.end.checked_add(1).ok_or(PlaybackError::Binding)?
            }
            RangeOutcome::Unsatisfiable => return Ok((outcome, None)),
        };
        let delivery = OrderedDelivery::new(selection.end - selection.start);
        let reader = DigestRange::from_enrolled(
            input.manifest,
            input.root,
            input.context,
            input.expected_record,
            input.retained_record,
            selection,
        )
        .map_err(PlaybackError::Frame)?;
        Ok((
            outcome,
            Some(Self {
                reader: Some(reader),
                plan: plan.ok_or(PlaybackError::Binding)?,
                locator,
                pins,
                delivery,
            }),
        ))
    }

    /// Check before dispatching; the transport must cancel in-flight reads at the deadline.
    pub fn next_chunk(
        &mut self,
        now: SystemTime,
    ) -> Result<Option<SealedRangeChunk>, PlaybackError> {
        if now >= self.pins.usable_until {
            self.reader = None;
            return Err(PlaybackError::Expired);
        }
        let reader = self.reader.as_ref().ok_or(PlaybackError::Stopped)?;
        if self.delivery.pending() {
            return Err(PlaybackError::Delivery);
        }
        reader
            .next_indices()
            .map(|(i, _)| self.plan.sealed_chunk(i).ok_or(PlaybackError::Binding))
            .transpose()
    }

    /// The transport binds bounded page and header reads to canonical keys,
    /// versions and encodings. `locator` is the actual media request locator.
    /// Success returns authenticated, trimmed plaintext; this is not delivery.
    pub async fn open_next(
        &mut self,
        locator: &S3Locator,
        response: S3RangeResponse<'_>,
        body: impl ReadbackBody,
        page: (&str, &[u8]),
        wire_header: Option<&[u8]>,
        mut now: impl FnMut() -> SystemTime,
    ) -> Result<OpenPart, PlaybackError> {
        let chunk = self.next_chunk(now())?.ok_or(PlaybackError::Stopped)?;
        // Take before any await: cancellation or failure fences the in-flight cursor.
        let reader = self.reader.take().ok_or(PlaybackError::Stopped)?;
        let index = chunk.selection.index;
        if locator != self.locator
            || !self.matches_page(chunk.selection.page, page.0)
            || (index == 0) != wire_header.is_some()
        {
            return Err(PlaybackError::Binding);
        }
        let sealed = read_s3_range_chunk(
            self.plan,
            index,
            self.locator,
            locator,
            response,
            body,
            || now() < self.pins.usable_until,
        )
        .await
        .map_err(PlaybackError::Response)?;
        self.open_part(reader, index, page.1, sealed, wire_header, now)
    }

    fn matches_page(&self, index: u32, version: &str) -> bool {
        self.pins
            .page_versions
            .get(index as usize)
            .is_some_and(|v| v == version)
    }

    fn open_part(
        &mut self,
        mut reader: DigestRange<'a>,
        index: u32,
        page: &[u8],
        sealed: Vec<u8>,
        wire_header: Option<&[u8]>,
        now: impl FnOnce() -> SystemTime,
    ) -> Result<OpenPart, PlaybackError> {
        let part = reader
            .open_next(index, page, sealed, wire_header)
            .map_err(PlaybackError::Frame)?;
        if now() >= self.pins.usable_until {
            return Err(PlaybackError::Expired);
        }
        self.delivery
            .offer(part.as_slice().len())
            .map_err(|_| PlaybackError::Delivery)?;
        self.reader = Some(reader);
        Ok(part)
    }

    /// Acknowledge only an ordered prefix actually accepted by the response
    /// writer. The caller must fence writes by the grant and read deadline;
    /// failed or uncertain writes are never acknowledged or retried.
    pub fn written(&mut self, bytes: usize) -> Result<(), PlaybackError> {
        if self.reader.is_none() || self.delivery.acknowledge(bytes).is_err() {
            self.reader = None;
            return Err(PlaybackError::Delivery);
        }
        Ok(())
    }

    /// Exact writer-accepted prefix, available for settlement even after failure.
    pub fn served_bytes(&self) -> u64 {
        self.delivery.accepted()
    }

    pub fn finish(self) -> Result<(), PlaybackError> {
        match self.reader {
            Some(reader) if self.delivery.complete() && reader.next_indices().is_none() => Ok(()),
            _ => Err(PlaybackError::Delivery),
        }
    }
}
