//! Unmounted FLAC preparation. The caller must supply frame boundaries from
//! complete CRC/PCM validation of this same immutable input; this module
//! checks geometry, not audio integrity. It cannot authorize storage or reads.

use std::ops::Range;

use crate::flac_preamble::FlacPreamble;

const MAX_COMMENT_BYTES: usize = 64 * 1024;
const MAX_SEEK_POINTS: usize = 65_536;
const CHANNEL_MASK: &str = "WAVEFORMATEXTENSIBLE_CHANNEL_MASK";

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
    offset: u64,
    samples: u16,
}

pub(crate) struct PreparedFlac<'a> {
    preamble: Vec<u8>,
    audio: &'a [u8],
    seek: FlacSeekIndex,
}

/// Sparse, whole-frame seek geometry for one exact FLAC plaintext layout.
/// This does not authenticate bytes or authorize a storage read.
pub struct FlacSeekIndex {
    points: Vec<SeekPoint>,
    audio_start: u64,
    audio_len: u64,
    sample_rate: u32,
    total_samples: u64,
}

impl PreparedFlac<'_> {
    /// New metadata followed by the unchanged input audio; no audio copy.
    pub(crate) fn parts(&self) -> (&[u8], &[u8]) {
        (&self.preamble, self.audio)
    }

    pub(crate) fn plaintext_range(&self, millis: Range<u64>) -> Option<Range<u64>> {
        self.seek.plaintext_range(millis)
    }
}

impl FlacSeekIndex {
    pub(crate) fn new(sample_rate: u32, audio_start: u64) -> Self {
        Self {
            points: Vec::new(),
            audio_start,
            audio_len: 0,
            sample_rate,
            total_samples: 0,
        }
    }

    /// Record the next contiguous validated frame. The end is an absolute
    /// plaintext offset, while embedded FLAC seek points are audio-relative.
    pub(crate) fn record_frame(&mut self, samples: u16, frame_end: u64) -> Result<(), ()> {
        if samples == 0 || self.sample_rate == 0 {
            return Err(());
        }
        let audio_len = frame_end
            .checked_sub(self.audio_start)
            .filter(|&len| len > self.audio_len)
            .ok_or(())?;
        let total_samples = self
            .total_samples
            .checked_add(u64::from(samples))
            .ok_or(())?;
        let interval = 5 * u64::from(self.sample_rate);
        if self
            .points
            .last()
            .is_none_or(|p| self.total_samples - p.sample >= interval)
        {
            if self.points.len() == MAX_SEEK_POINTS {
                return Err(());
            }
            self.points.try_reserve(1).map_err(|_| ())?;
            self.points.push(SeekPoint {
                sample: self.total_samples,
                offset: self.audio_len,
                samples,
            });
        }
        self.audio_len = audio_len;
        self.total_samples = total_samples;
        Ok(())
    }

    /// Duration of the validated PCM, rounded up to the next millisecond.
    pub fn duration_ms(&self) -> Option<u64> {
        if self.sample_rate == 0 || self.total_samples == 0 {
            return None;
        }
        let millis = self.total_samples.checked_mul(1000)?;
        Some(millis.div_ceil(u64::from(self.sample_rate)))
    }

    /// Whole-frame, half-open range in the indexed plaintext. Seek back to
    /// the anchor at or before the start, and stop before the first anchor
    /// at or after the exclusive end (or at EOF).
    pub fn plaintext_range(&self, millis: Range<u64>) -> Option<Range<u64>> {
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
            .checked_sub(1)?;
        let end = self
            .points
            .partition_point(|p| u128::from(p.sample) < limit);
        let end_offset = self.points.get(end).map_or(self.audio_len, |p| p.offset);
        Some(
            self.audio_start.checked_add(self.points[start].offset)?
                ..self.audio_start.checked_add(end_offset)?,
        )
    }
}

pub(crate) fn prepare<'a>(
    input: &'a [u8],
    tags: &[(&str, &str)],
    validated_frames: impl IntoIterator<Item = FrameSpan>,
) -> Result<PreparedFlac<'a>, PrepareError> {
    let mut source = FlacPreamble::default();
    let (info, audio) = source
        .push(input)
        .map_err(|()| PrepareError::Preamble)?
        .ok_or(PrepareError::Preamble)?;
    let mut seek = FlacSeekIndex::new(info.sample_rate, 0);
    for frame in validated_frames {
        if frame.bytes.start as u64 != seek.audio_len
            || frame.bytes.end <= frame.bytes.start
            || frame.bytes.end > audio.len()
            || !source
                .frame_sizes()
                .contains(&((frame.bytes.end - frame.bytes.start) as u64))
            || frame.first_sample != seek.total_samples
            || frame.samples == 0
            || frame.samples > info.max_block
            // RFC 9639 §8.2 exempts the final frame from the minimum.
            || (frame.samples < info.min_block && frame.bytes.end < audio.len())
        {
            return Err(PrepareError::FrameGeometry);
        }
        seek.record_frame(frame.samples, frame.bytes.end as u64)
            .map_err(|()| PrepareError::SeekLimit)?;
        if seek.total_samples >= 1 << 36 {
            return Err(PrepareError::FrameGeometry);
        }
    }
    if seek.points.is_empty()
        || seek.audio_len != audio.len() as u64
        || (info.total_samples != 0 && info.total_samples != seek.total_samples)
    {
        return Err(PrepareError::FrameGeometry);
    }

    // Emit only STREAMINFO, the replacement comment, and a rebuilt SEEKTABLE.
    // In particular, source PICTURE blocks and foreign offsets never survive.
    let mask = source_channel_mask(&input[42..input.len() - audio.len()], info.channels)
        .ok_or(PrepareError::Preamble)?;
    let comments = vorbis_comments(tags, mask)?;
    let mut preamble = input[..42].to_vec();
    preamble[4] = 0;
    block_header(&mut preamble, 4, comments.len());
    preamble.extend_from_slice(&comments);
    block_header(&mut preamble, 0x80 | 3, seek.points.len() * 18);
    for point in &seek.points {
        preamble.extend_from_slice(&point.sample.to_be_bytes());
        preamble.extend_from_slice(&point.offset.to_be_bytes());
        preamble.extend_from_slice(&point.samples.to_be_bytes());
    }
    seek.audio_start = preamble.len() as u64;
    Ok(PreparedFlac {
        preamble,
        audio,
        seek,
    })
}

fn block_header(out: &mut Vec<u8>, flags: u8, len: usize) {
    out.push(flags);
    out.extend_from_slice(&(len as u32).to_be_bytes()[1..]);
}

fn take_vorbis_field<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let (len, rest) = bytes.split_first_chunk::<4>()?;
    let (field, rest) = rest.split_at_checked(u32::from_le_bytes(*len) as usize)?;
    *bytes = rest;
    Some(field)
}

// Layout describes the unchanged frames, not the replacement catalogue tags.
fn source_channel_mask(mut blocks: &[u8], channels: u8) -> Option<Option<u32>> {
    let mut mask = None;
    let mut seen_comment = false;
    while let Some(([kind, a, b, c], rest)) = blocks.split_first_chunk::<4>() {
        let (mut payload, rest) =
            rest.split_at_checked(u32::from_be_bytes([0, *a, *b, *c]) as usize)?;
        blocks = rest;
        if kind & 127 != 4 {
            continue;
        }
        if seen_comment {
            return None;
        }
        seen_comment = true;
        take_vorbis_field(&mut payload)?; // vendor
        let (count, mut fields) = payload.split_first_chunk::<4>()?;
        for _ in 0..u32::from_le_bytes(*count) {
            let field = take_vorbis_field(&mut fields)?;
            let mut parts = field.splitn(2, |&byte| byte == b'=');
            if !parts.next()?.eq_ignore_ascii_case(CHANNEL_MASK.as_bytes()) {
                continue;
            }
            let (prefix, hex) = parts.next()?.split_at_checked(2)?;
            if !prefix.eq_ignore_ascii_case(b"0x") || !hex.first()?.is_ascii_hexdigit() {
                return None;
            }
            let value = u32::from_str_radix(str::from_utf8(hex).ok()?, 16).ok()?;
            if value >= 1 << 18
                || value.count_ones() > u32::from(channels)
                || mask.replace(value).is_some()
            {
                return None;
            }
        }
        if !fields.is_empty() {
            return None;
        }
    }
    Some(mask)
}

fn vorbis_comments(tags: &[(&str, &str)], mask: Option<u32>) -> Result<Vec<u8>, PrepareError> {
    const VENDOR: &[u8] = b"NNTune";
    let count = tags.len() + usize::from(mask.is_some());
    if count > 256
        || tags
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case(CHANNEL_MASK))
    {
        return Err(PrepareError::Tags);
    }
    let mut out = (VENDOR.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(VENDOR);
    out.extend_from_slice(&(count as u32).to_le_bytes());
    let layout = mask.map(|value| format!("0x{value:x}"));
    for (key, value) in tags
        .iter()
        .copied()
        .chain(layout.as_deref().map(|value| (CHANNEL_MASK, value)))
    {
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

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn rejects_nonadvancing_or_empty_seek_frames_without_changing_index() {
        let mut seek = FlacSeekIndex::new(1, 42);
        assert_eq!(seek.record_frame(5, 53), Ok(()));
        for (samples, end) in [(0, 54), (5, 53), (5, 52), (5, 41)] {
            assert_eq!(seek.record_frame(samples, end), Err(()));
            assert_eq!(seek.duration_ms(), Some(5_000));
            assert_eq!(seek.plaintext_range(0..5_000), Some(42..53));
        }
        assert_eq!(seek.record_frame(5, u64::MAX), Ok(()));
        assert_eq!(seek.plaintext_range(5_000..10_000), Some(53..u64::MAX));
    }

    fn input(comments: &[&[u8]]) -> Vec<u8> {
        let mut bytes = b"fLaC\x80\0\0\x22\0\x10\0\x10\0\0\x0b\0\0\x0b".to_vec();
        bytes.extend_from_slice(&((44_100_u64 << 44) | (7 << 36) | 16).to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        for (index, payload) in comments.iter().enumerate() {
            bytes[4] = 0;
            let last = u8::from(index + 1 == comments.len()) << 7;
            block_header(&mut bytes, last | 4, payload.len());
            bytes.extend_from_slice(payload);
        }
        bytes.extend_from_slice(&[255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221]);
        bytes
    }

    const FRAME: FrameSpan = FrameSpan {
        first_sample: 0,
        samples: 16,
        bytes: 0..11,
    };

    #[test]
    fn refuses_frame_spans_outside_declared_byte_bounds() {
        assert!(prepare(&input(&[]), &[], [FRAME]).is_ok());
        for (min, max) in [(12, 0), (0, 10)] {
            let mut bytes = input(&[]);
            bytes[14] = min;
            bytes[17] = max;
            assert_eq!(
                prepare(&bytes, &[], [FRAME]).err(),
                Some(PrepareError::FrameGeometry)
            );
        }
    }

    #[test]
    fn refuses_ambiguous_source_comment_boundaries() {
        let empty = &[0; 8];
        assert!(prepare(&input(&[empty]), &[], [FRAME]).is_ok());
        for comments in [&[empty.as_slice(), empty][..], &[&[0; 9][..]]] {
            assert_eq!(
                prepare(&input(comments), &[], [FRAME]).err(),
                Some(PrepareError::Preamble)
            );
        }
    }
}
