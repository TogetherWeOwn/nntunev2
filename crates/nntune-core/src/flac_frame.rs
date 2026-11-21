//! Unmounted boundary reader for one independent-channel FLAC frame.

use crate::flac_frame_footer::{FrameCrc, FrameFooter};
use crate::flac_frame_header::FrameHeader;
use crate::flac_preamble::StreamInfo;
use crate::flac_samples::{Samples, SubframeSamples};
use crate::flac_subframe::{BitInput, SubframeHeaderReader};

pub(crate) struct FrameReader {
    header: FrameHeader,
    channels_left: u8,
    channel: SubframeHeaderReader,
    samples: Option<SubframeSamples>,
    bits: BitInput,
    crc: FrameCrc,
    footer: Option<FrameFooter>,
    failed: bool,
}

impl FrameReader {
    /// The caller supplies the exact frame boundary and retains an incomplete
    /// header (at most 16 bytes). This slice does not handle coupled stereo.
    pub(crate) fn start(input: &mut &[u8], info: &StreamInfo) -> Result<Option<Self>, ()> {
        let Some(header) = FrameHeader::parse(input, info)? else {
            return Ok(None);
        };
        if header.channel_assignment > 7 {
            return Err(());
        }
        let channels_left = header.channel_assignment + 1;
        let channel = SubframeHeaderReader::new(header.bits_per_sample)?;
        let (prefix, rest) = input.split_at(header.len);
        let mut crc = FrameCrc::default();
        crc.update(prefix);
        *input = rest;
        Ok(Some(Self {
            header,
            channels_left,
            channel,
            samples: None,
            bits: BitInput::default(),
            crc,
            footer: None,
            failed: false,
        }))
    }

    /// Decode channels only to locate the footer; this neither emits PCM nor
    /// checks stream continuity. A completed frame leaves the next byte unread.
    pub(crate) fn read(&mut self, input: &mut &[u8]) -> Result<bool, ()> {
        if self.failed {
            return Err(());
        }
        let result = self.read_body(input);
        self.failed = result.is_err();
        result
    }

    fn read_body(&mut self, input: &mut &[u8]) -> Result<bool, ()> {
        if self.footer.is_none() {
            let before = *input;
            let complete = self.read_channels(input)?;
            self.crc.update(&before[..before.len() - input.len()]);
            if !complete {
                return Ok(false);
            }
            self.footer = Some(std::mem::take(&mut self.crc).into_footer(&mut self.bits)?);
        }
        self.footer.as_mut().ok_or(())?.read(input)
    }

    fn read_channels(&mut self, input: &mut &[u8]) -> Result<bool, ()> {
        while self.channels_left != 0 {
            if self.samples.is_none() {
                let Some(header) = self.channel.read(&mut self.bits, input)? else {
                    return Ok(false);
                };
                self.samples = Some(SubframeSamples::new(header, self.header.block_size)?);
            }
            let samples = self.samples.as_mut().ok_or(())?;
            match samples.read(&mut self.bits, input)? {
                Samples::Sample(_) => {}
                Samples::NeedInput => return Ok(false),
                Samples::End => {
                    self.channels_left -= 1;
                    self.samples = None;
                    self.channel = SubframeHeaderReader::new(self.header.bits_per_sample)?;
                }
            }
        }
        Ok(true)
    }

    pub(crate) fn finish(self) -> Result<FrameHeader, ()> {
        if self.failed {
            return Err(());
        }
        self.footer.ok_or(())?.finish_at_eof()?;
        Ok(self.header)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flac_frame_header::Position;

    // Synthetic 192-sample mono constant frames with 12-bit samples. Both
    // CRCs were pinned independently via polynomial long division.
    const FIRST: [u8; 11] = [0xff, 0xf8, 0x19, 4, 0, 0x46, 0, 0x5a, 0xb0, 0x7f, 0x33];
    const SECOND: [u8; 11] = [0xff, 0xf8, 0x19, 4, 1, 0x41, 0, 0x32, 0x10, 0x60, 0x8d];
    const INFO: StreamInfo = StreamInfo {
        min_block: 192,
        max_block: 192,
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 12,
        total_samples: 384,
        pcm_md5: [0; 16],
    };

    #[test]
    fn preserves_following_frame_at_every_body_split() {
        let wire = [FIRST, SECOND].concat();
        for split in 6..=FIRST.len() {
            let (mut prefix, mut rest) = wire.split_at(split);
            let mut frame = FrameReader::start(&mut prefix, &INFO).unwrap().unwrap();
            assert_eq!(frame.read(&mut prefix), Ok(split == FIRST.len()));
            assert!(prefix.is_empty());
            if split < FIRST.len() {
                assert_eq!(frame.read(&mut rest), Ok(true));
            }
            assert_eq!(rest, SECOND);
            assert_eq!(frame.finish().unwrap().position, Position::Frame(0));
            let mut next = FrameReader::start(&mut rest, &INFO).unwrap().unwrap();
            assert_eq!(next.read(&mut rest), Ok(true));
            assert_eq!(next.finish().unwrap().position, Position::Frame(1));
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn shares_unaligned_cursor_across_channels() {
        let wire = [
            0xff, 0xf8, 0x19, 0x14, 0, 0x11, 0, 0x5a, 0xb0, 0x0a, 0x34, 0x56, 0x21,
        ];
        let info = StreamInfo {
            channels: 2,
            ..INFO
        };
        let mut header = &wire[..6];
        let mut frame = FrameReader::start(&mut header, &info).unwrap().unwrap();
        let body = &wire[6..];
        for (index, byte) in body.iter().enumerate() {
            let mut input = std::slice::from_ref(byte);
            assert_eq!(frame.read(&mut input), Ok(index + 1 == body.len()));
            assert!(input.is_empty());
        }
        assert_eq!(frame.finish().unwrap().channel_assignment, 1);
    }

    #[test]
    fn rejects_each_truncated_prefix_and_sticks_on_corrupt_frames() {
        for len in 1..SECOND.len() {
            let wire = [FIRST.as_slice(), &SECOND[..len]].concat();
            let mut prefix = wire.as_slice();
            let mut first = FrameReader::start(&mut prefix, &INFO).unwrap().unwrap();
            assert_eq!(first.read(&mut prefix), Ok(true));
            assert!(first.finish().is_ok());
            match FrameReader::start(&mut prefix, &INFO).unwrap() {
                None => assert_eq!(prefix, &SECOND[..len]),
                Some(mut frame) => {
                    assert_eq!(frame.read(&mut prefix), Ok(false));
                    assert!(frame.finish().is_err());
                }
            }
        }
        let mut wrong_crc = SECOND;
        wrong_crc[10] ^= 1;
        // Matching CRC for the same frame with a nonzero padding bit.
        let bad_padding = [0xff, 0xf8, 0x19, 4, 1, 0x41, 0, 0x32, 0x11, 0xe0, 0x88];
        for bad in [wrong_crc, bad_padding] {
            let mut input = bad.as_slice();
            let mut frame = FrameReader::start(&mut input, &INFO).unwrap().unwrap();
            assert_eq!(frame.read(&mut input), Err(()));
            let mut suffix = FIRST.as_slice();
            assert_eq!(frame.read(&mut suffix), Err(()));
            assert_eq!(suffix, FIRST);
            assert!(frame.finish().is_err());
        }
    }
}
