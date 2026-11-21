//! Unmounted FLAC preparation. The caller must supply frame boundaries from
//! complete CRC/PCM validation of this same immutable input; this module
//! checks geometry, not audio integrity. It cannot authorize storage or reads.

use std::ops::Range;

use crate::flac_preamble::FlacPreamble;

const MAX_COMMENT_BYTES: usize = 64 * 1024;
const MAX_SEEK_POINTS: usize = 65_536;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PrepareError {
    Preamble,
    Tags,
    FrameGeometry,
    SeekLimit,
}

pub(crate) struct FrameSpan {
    pub(crate) first_sample: u64,
    pub(crate) samples: u16,
    /// Half-open offsets relative to the first audio frame, not the input file.
    pub(crate) bytes: Range<usize>,
}

struct SeekPoint {
    sample: u64,
    offset: usize,
    samples: u16,
}

pub(crate) struct PreparedFlac<'a> {
    preamble: Vec<u8>,
    audio: &'a [u8],
    points: Vec<SeekPoint>,
    sample_rate: u32,
    total_samples: u64,
}

impl PreparedFlac<'_> {
    /// New metadata followed by the unchanged input audio; no audio copy.
    pub(crate) fn parts(&self) -> (&[u8], &[u8]) {
        (&self.preamble, self.audio)
    }

    /// Whole-frame, half-open range in the *prepared plaintext*. Seek back
    /// to the anchor at or before the start, and stop before the first anchor
    /// at or after the exclusive end (or at EOF). No ciphertext math here.
    pub(crate) fn plaintext_range(&self, millis: Range<u64>) -> Option<Range<u64>> {
        if millis.start >= millis.end {
            return None;
        }
        let rate = u128::from(self.sample_rate);
        let first = u128::from(millis.start) * rate / 1000;
        if first >= u128::from(self.total_samples) {
            return None;
        }
        let limit = (u128::from(millis.end) * rate).div_ceil(1000);
        let start = self
            .points
            .partition_point(|p| u128::from(p.sample) <= first)
            - 1;
        let end = self
            .points
            .partition_point(|p| u128::from(p.sample) < limit);
        let base = self.preamble.len() as u64;
        let end_offset = self.points.get(end).map_or(self.audio.len(), |p| p.offset);
        Some(base + self.points[start].offset as u64..base + end_offset as u64)
    }
}

pub(crate) fn prepare<'a>(
    input: &'a [u8],
    tags: &[(&str, &str)],
    validated_frames: impl IntoIterator<Item = FrameSpan>,
) -> Result<PreparedFlac<'a>, PrepareError> {
    let (info, audio) = FlacPreamble::default()
        .push(input)
        .map_err(|()| PrepareError::Preamble)?
        .ok_or(PrepareError::Preamble)?;
    let mut points: Vec<SeekPoint> = Vec::new();
    let (mut bytes, mut samples) = (0, 0_u64);
    let interval = 5 * u64::from(info.sample_rate);
    for frame in validated_frames {
        if frame.bytes.start != bytes
            || frame.bytes.end <= bytes
            || frame.bytes.end > audio.len()
            || frame.first_sample != samples
            || frame.samples == 0
            || frame.samples > info.max_block
            // RFC 9639 §8.2 exempts the final frame from the minimum.
            || (frame.samples < info.min_block && frame.bytes.end < audio.len())
        {
            return Err(PrepareError::FrameGeometry);
        }
        if points.last().is_none_or(|p| samples - p.sample >= interval) {
            if points.len() == MAX_SEEK_POINTS {
                return Err(PrepareError::SeekLimit);
            }
            points.push(SeekPoint {
                sample: samples,
                offset: bytes,
                samples: frame.samples,
            });
        }
        bytes = frame.bytes.end;
        samples += u64::from(frame.samples);
        if samples >= 1 << 36 {
            return Err(PrepareError::FrameGeometry);
        }
    }
    if points.is_empty()
        || bytes != audio.len()
        || (info.total_samples != 0 && info.total_samples != samples)
    {
        return Err(PrepareError::FrameGeometry);
    }

    // Emit only STREAMINFO, the replacement comment, and a rebuilt SEEKTABLE.
    // In particular, source PICTURE blocks and foreign offsets never survive.
    let comments = vorbis_comments(tags)?;
    let mut preamble = input[..42].to_vec();
    preamble[4] = 0;
    block_header(&mut preamble, 4, comments.len());
    preamble.extend_from_slice(&comments);
    block_header(&mut preamble, 0x80 | 3, points.len() * 18);
    for point in &points {
        preamble.extend_from_slice(&point.sample.to_be_bytes());
        preamble.extend_from_slice(&(point.offset as u64).to_be_bytes());
        preamble.extend_from_slice(&point.samples.to_be_bytes());
    }
    Ok(PreparedFlac {
        preamble,
        audio,
        points,
        sample_rate: info.sample_rate,
        total_samples: samples,
    })
}

fn block_header(out: &mut Vec<u8>, flags: u8, len: usize) {
    out.push(flags);
    out.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
}

fn vorbis_comments(tags: &[(&str, &str)]) -> Result<Vec<u8>, PrepareError> {
    const VENDOR: &[u8] = b"NNTune";
    if tags.len() > 256 {
        return Err(PrepareError::Tags);
    }
    let mut out = (VENDOR.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(VENDOR);
    out.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for &(key, value) in tags {
        let size = key.len().saturating_add(value.len()).saturating_add(1);
        if size.saturating_add(4) > MAX_COMMENT_BYTES - out.len()
            || key.is_empty()
            || !key.bytes().all(|b| (0x20..=0x7d).contains(&b) && b != b'=')
            || ["METADATA_BLOCK_PICTURE", "COVERART", "COVERARTMIME"]
                .iter()
                .any(|art_key| key.eq_ignore_ascii_case(art_key))
            || value.contains('\0')
        {
            return Err(PrepareError::Tags);
        }
        out.extend_from_slice(&(size as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.push(b'=');
        out.extend_from_slice(value.as_bytes());
    }
    Ok(out)
}

#[cfg(test)]
#[path = "flac_prepare_tests.rs"]
mod tests;
