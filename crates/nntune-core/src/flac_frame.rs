//! Unmounted boundary reader for one FLAC frame.

use crate::flac_frame_footer::{FrameCrc, FrameFooter};
use crate::flac_frame_header::FrameHeader;
use crate::flac_preamble::StreamInfo;
use crate::flac_samples::{Samples, SubframeSamples};
use crate::flac_stereo::Stereo;
use crate::flac_subframe::{BitInput, SubframeHeaderReader};

pub(crate) struct FrameReader {
    header: FrameHeader,
    channels_left: u8,
    channel: SubframeHeaderReader,
    samples: Option<SubframeSamples>,
    stereo: Option<Stereo>,
    first_channel: Vec<i64>,
    pair_index: usize,
    bits: BitInput,
    crc: FrameCrc,
    footer: Option<FrameFooter>,
    failed: bool,
}

impl FrameReader {
    /// The caller supplies the exact frame boundary and retains an incomplete
    /// header (at most 16 bytes).
    pub(crate) fn start(input: &mut &[u8], info: &StreamInfo) -> Result<Option<Self>, ()> {
        let Some(header) = FrameHeader::parse(input, info)? else {
            return Ok(None);
        };
        let mut first_channel = Vec::new();
        let (channels_left, first_width, stereo) = if header.channel_assignment <= 7 {
            (header.channel_assignment + 1, header.bits_per_sample, None)
        } else {
            let stereo = Stereo::new(header.channel_assignment, header.bits_per_sample)?;
            // Subframes are serialized, not interleaved. Retain only the
            // first one, bounded by the checked u16 block size.
            first_channel
                .try_reserve_exact(usize::from(header.block_size))
                .map_err(|_| ())?;
            (2, stereo.coded_widths()[0], Some(stereo))
        };
        let channel = SubframeHeaderReader::new(first_width)?;
        let (prefix, rest) = input.split_at(header.len);
        let mut crc = FrameCrc::default();
        crc.update(prefix);
        *input = rest;
        Ok(Some(Self {
            header,
            channels_left,
            channel,
            samples: None,
            stereo,
            first_channel,
            pair_index: 0,
            bits: BitInput::default(),
            crc,
            footer: None,
            failed: false,
        }))
    }

    /// Decode subframes, check coupled PCM ranges, and verify the footer.
    /// This emits no PCM and leaves the byte after the frame unread.
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
                Samples::Sample(sample) => {
                    if let Some(stereo) = &self.stereo {
                        if self.channels_left == 2 {
                            // SubframeSamples emits exactly block_size samples.
                            self.first_channel.push(sample);
                        } else {
                            stereo.restore(
                                *self.first_channel.get(self.pair_index).ok_or(())?,
                                sample,
                            )?;
                            self.pair_index += 1;
                        }
                    }
                }
                Samples::NeedInput => return Ok(false),
                Samples::End => {
                    self.channels_left -= 1;
                    self.samples = None;
                    if self.channels_left != 0 {
                        let width = self
                            .stereo
                            .as_ref()
                            .map_or(self.header.bits_per_sample, |stereo| {
                                stereo.coded_widths()[1]
                            });
                        self.channel = SubframeHeaderReader::new(width)?;
                    }
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

    fn coupled_info(bits_per_sample: u8) -> StreamInfo {
        StreamInfo {
            min_block: 16,
            max_block: 16,
            channels: 2,
            bits_per_sample,
            total_samples: 16,
            ..INFO
        }
    }

    #[test]
    fn checks_coupled_pairs_at_every_body_split() {
        // Left/side alternates opposite PCM extrema; pairing either side
        // with the wrong left sample overflows. Side/right and mid/side
        // use opposite i32 extrema, requiring signed 33-bit coded sides.
        let left_side = [
            0xff, 0xf8, 0x69, 0x80, 0x00, 0x0f, 0x6a, 0x02, 0x87, 0x87, 0x87, 0x87, 0x87, 0x87,
            0x87, 0x87, 0x02, 0x8b, 0xe2, 0xf8, 0xbe, 0x2f, 0x8b, 0xe2, 0xf8, 0xbe, 0x2f, 0x11,
            0x44,
        ];
        let side_right = [
            0xff, 0xf8, 0x69, 0x90, 0x00, 0x0f, 0xc8, 0x00, 0x7f, 0xff, 0xff, 0xff, 0x80, 0x40,
            0x00, 0x00, 0x00, 0x00, 0x41, 0x10,
        ];
        let mid_side = [
            0xff, 0xf8, 0x69, 0xa0, 0x00, 0x0f, 0x29, 0x00, 0xff, 0xff, 0xff, 0xff, 0x00, 0x80,
            0x00, 0x00, 0x00, 0x80, 0x35, 0x36,
        ];
        for (wire, depth, assignment) in [
            (left_side.as_slice(), 4, 8),
            (side_right.as_slice(), 32, 9),
            (mid_side.as_slice(), 32, 10),
        ] {
            let joined = [wire, wire].concat();
            for split in 7..=wire.len() {
                let (mut prefix, mut rest) = joined.split_at(split);
                let mut frame = FrameReader::start(&mut prefix, &coupled_info(depth))
                    .unwrap()
                    .unwrap();
                assert_eq!(frame.read(&mut prefix), Ok(split == wire.len()));
                assert!(prefix.is_empty());
                if split < wire.len() {
                    assert_eq!(frame.read(&mut rest), Ok(true));
                }
                assert_eq!(rest, wire);
                assert_eq!(frame.finish().unwrap().channel_assignment, assignment);
            }
        }
    }

    #[test]
    fn refuses_coupled_pcm_overflow_even_with_matching_crc() {
        // The inputs fit their coded widths in all three assignments; at
        // least one reconstructed output is outside the signed 4-bit range.
        let info = coupled_info(4);
        for wire in [
            [
                0xff, 0xf8, 0x69, 0x80, 1, 0x0f, 0x7f, 0, 0x80, 0, 0x80, 0x16, 0x2c,
            ],
            [
                0xff, 0xf8, 0x69, 0x90, 1, 0x0f, 0xdd, 0, 0x80, 0, 0, 0xa6, 0xb9,
            ],
            [
                0xff, 0xf8, 0x69, 0xa0, 1, 0x0f, 0x3c, 0, 0x70, 0x0f, 0x80, 0x26, 0xa8,
            ],
        ] {
            let mut input = wire.as_slice();
            let mut frame = FrameReader::start(&mut input, &info).unwrap().unwrap();
            assert_eq!(frame.read(&mut input), Err(()));
            assert_eq!(input, &wire[11..]); // Refusal occurred before the CRC.
            assert_eq!(frame.read(&mut input), Err(()));
            assert_eq!(input, &wire[11..]);
            assert!(frame.finish().is_err());
        }
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
