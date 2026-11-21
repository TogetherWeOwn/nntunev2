//! Native FLAC frame padding and footer (RFC 9639 §9.3).

use crate::flac_subframe::BitInput;

#[derive(Default)]
pub(crate) struct FrameCrc(u16);

impl FrameCrc {
    /// Feed each consumed byte once, starting at the sync code, including the
    /// header CRC and any final byte containing padding, but not the footer.
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 ^= u16::from(byte) << 8;
            for _ in 0..8 {
                self.0 = (self.0 << 1) ^ if self.0 & 0x8000 != 0 { 0x8005 } else { 0 };
            }
        }
    }

    /// Call only after the last subframe is complete and its consumed bytes
    /// have been fed to the CRC. The cursor already holds any alignment bits.
    pub(crate) fn into_footer(self, bits: &mut BitInput) -> Result<FrameFooter, ()> {
        bits.consume_zero_padding()?;
        Ok(FrameFooter(FooterState::High(self.0.to_be_bytes())))
    }
}

enum FooterState {
    High([u8; 2]),
    Low(u8),
    Done,
    Invalid,
}

pub(crate) struct FrameFooter(FooterState);

impl FrameFooter {
    /// Read exactly the two CRC bytes. `Ok(false)` needs more input; a
    /// mismatch or another read after a terminal result is an error.
    pub(crate) fn read(&mut self, input: &mut &[u8]) -> Result<bool, ()> {
        loop {
            let (expected, next) = match self.0 {
                FooterState::High([high, low]) => (high, FooterState::Low(low)),
                FooterState::Low(low) => (low, FooterState::Done),
                FooterState::Done | FooterState::Invalid => return Err(()),
            };
            let Some((&byte, rest)) = input.split_first() else {
                return Ok(false);
            };
            if byte != expected {
                self.0 = FooterState::Invalid;
                return Err(());
            }
            *input = rest;
            self.0 = next;
            if matches!(self.0, FooterState::Done) {
                return Ok(true);
            }
        }
    }

    /// At end of input, an incomplete or invalid footer is an error.
    pub(crate) fn finish_at_eof(self) -> Result<(), ()> {
        match self.0 {
            FooterState::Done => Ok(()),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 9639 Appendix D.2: second audio frame, 0xcc..=0xe2, with two
    // alignment bits in 0x7c. The published footer is 0x1330.
    const PADDED: [u8; 21] = [
        0xff, 0xf8, 0x69, 0x18, 0x01, 0x02, 0xa4, 0x02, 0xc3, 0x82, 0xc4, 0x0b, 0xc1, 0x4a, 0x03,
        0xee, 0x48, 0xdd, 0x03, 0xb6, 0x7c,
    ];
    const CRC: [u8; 2] = [0x13, 0x30];

    fn padded_footer(body: &[u8; 21], split: usize) -> Result<FrameFooter, ()> {
        let mut crc = FrameCrc::default();
        crc.update(&body[..split]);
        crc.update(&body[split..]);
        // Advance the real bit cursor to the published subframe endpoint.
        let mut bits = BitInput::default();
        let mut last = &body[20..];
        assert_eq!(bits.read(&mut last, 6), Ok(Some(0b011111)));
        assert!(last.is_empty());
        let footer = crc.into_footer(&mut bits)?;
        assert_eq!(bits.read(&mut [0xa5].as_slice(), 8), Ok(Some(0xa5)));
        Ok(footer)
    }

    #[test]
    fn published_padded_crc_survives_splits_and_leaves_following_bytes() {
        let wire = [CRC[0], CRC[1], 0xff, 0xf8, 0x11];
        for body_split in 0..=PADDED.len() {
            for footer_split in 0..=CRC.len() {
                let mut footer = padded_footer(&PADDED, body_split).unwrap();
                let (mut first, mut rest) = wire.split_at(footer_split);
                assert_eq!(footer.read(&mut first), Ok(footer_split == CRC.len()));
                assert!(first.is_empty());
                if footer_split != CRC.len() {
                    assert_eq!(footer.read(&mut rest), Ok(true));
                }
                assert_eq!(rest, &wire[2..]);
                assert_eq!(footer.read(&mut rest), Err(()));
                assert_eq!(rest, &wire[2..]);
                assert_eq!(footer.finish_at_eof(), Ok(()));
            }
        }
    }

    #[test]
    fn published_byte_aligned_frame_needs_no_padding() {
        // RFC 9639 Appendix D.1: frame 0x2a..=0x38, with footer 0xaa9a.
        let mut crc = FrameCrc::default();
        crc.update(&[
            0xff, 0xf8, 0x69, 0x18, 0x00, 0x00, 0xbf, 0x03, 0x58, 0xfd, 0x03, 0x12, 0x8b,
        ]);
        let mut bits = BitInput::default();
        assert_eq!(bits.read(&mut [0x8b].as_slice(), 8), Ok(Some(0x8b)));
        let mut footer = crc.into_footer(&mut bits).unwrap();
        assert_eq!(footer.read(&mut [0xaa, 0x9a].as_slice()), Ok(true));
    }

    #[test]
    fn rejects_each_nonzero_padding_bit_before_reading_crc() {
        for mask in [1, 2] {
            let mut body = PADDED;
            body[20] |= mask;
            assert!(padded_footer(&body, body.len()).is_err());
        }
    }

    #[test]
    fn rejects_buffered_whole_byte_as_padding() {
        let mut bits = BitInput::default();
        assert_eq!(bits.read(&mut [0].as_slice(), 9), Ok(None));
        assert!(FrameCrc::default().into_footer(&mut bits).is_err());
    }

    #[test]
    fn rejects_wrong_and_truncated_crc() {
        for wrong in [[CRC[0] ^ 1, CRC[1]], [CRC[0], CRC[1] ^ 1]] {
            let mut footer = padded_footer(&PADDED, PADDED.len()).unwrap();
            assert_eq!(footer.read(&mut wrong.as_slice()), Err(()));
            assert_eq!(footer.read(&mut CRC.as_slice()), Err(()));
            assert_eq!(footer.finish_at_eof(), Err(()));
        }
        for len in 0..CRC.len() {
            let mut footer = padded_footer(&PADDED, PADDED.len()).unwrap();
            assert_eq!(footer.read(&mut &CRC[..len]), Ok(false));
            assert_eq!(footer.finish_at_eof(), Err(()));
        }
    }
}
