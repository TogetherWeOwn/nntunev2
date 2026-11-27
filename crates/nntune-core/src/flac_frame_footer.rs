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

    // CRC-8 (x^8 + x^2 + x + 1) and CRC-16 (x^16 + x^15 + x^2 + 1)
    // pins were derived by polynomial long division, not FrameCrc.
    const PADDED_CRC: [u8; 2] = 0x7f33u16.to_be_bytes();
    const ALIGNED_CRC: [u8; 2] = 0x5623u16.to_be_bytes();

    fn constant_frame_body(sample_size_code: u8, header_crc: u8, packed_sample: u16) -> [u8; 9] {
        // Original synthetic mono frame: fixed blocking, 192 samples at
        // 44.1 kHz, frame number 0, one constant subframe. The sample is
        // left-aligned in its two bytes and any low bits are zero padding.
        let [high, low] = packed_sample.to_be_bytes();
        [
            0xff,
            0xf8,
            0x19,
            sample_size_code << 1,
            0,
            header_crc,
            0, // Constant subframe header, no wasted bits.
            high,
            low,
        ]
    }

    fn padded_frame_body() -> [u8; 9] {
        constant_frame_body(2, 0x46, 0x05ab << 4) // 12-bit sample, four padding bits.
    }

    fn padded_footer(body: &[u8; 9], split: usize) -> Result<FrameFooter, ()> {
        let mut crc = FrameCrc::default();
        crc.update(&body[..split]);
        crc.update(&body[split..]);
        // Read the 12-bit sample with the real cursor, leaving its four pad bits.
        let mut bits = BitInput::default();
        let mut sample = &body[7..];
        assert_eq!(bits.read(&mut sample, 12), Ok(Some(0x05ab)));
        assert!(sample.is_empty());
        let footer = crc.into_footer(&mut bits)?;
        assert_eq!(bits.read(&mut [0xa5].as_slice(), 8), Ok(Some(0xa5)));
        Ok(footer)
    }

    #[test]
    fn padded_crc_survives_splits_and_leaves_following_bytes() {
        let body = padded_frame_body();
        let wire = [PADDED_CRC[0], PADDED_CRC[1], 0xff, 0xf8, 0x11];
        for body_split in 0..=body.len() {
            for footer_split in 0..=PADDED_CRC.len() {
                let mut footer = padded_footer(&body, body_split).unwrap();
                let (mut first, mut rest) = wire.split_at(footer_split);
                assert_eq!(
                    footer.read(&mut first),
                    Ok(footer_split == PADDED_CRC.len())
                );
                assert!(first.is_empty());
                if footer_split != PADDED_CRC.len() {
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
    fn byte_aligned_frame_needs_no_padding() {
        let body = constant_frame_body(4, 0xba, 0x2ec7); // 16-bit sample.
        let mut crc = FrameCrc::default();
        crc.update(&body);
        let mut bits = BitInput::default();
        assert_eq!(bits.read(&mut &body[7..], 16), Ok(Some(0x2ec7)));
        let mut footer = crc.into_footer(&mut bits).unwrap();
        assert_eq!(footer.read(&mut ALIGNED_CRC.as_slice()), Ok(true));
    }

    #[test]
    fn rejects_each_nonzero_padding_bit_before_reading_crc() {
        for mask in [1, 2, 4, 8] {
            let mut body = padded_frame_body();
            *body.last_mut().unwrap() |= mask;
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
    fn rejects_unfinished_fields_even_without_a_buffered_byte() {
        let mut bits = BitInput::default();
        let mut input = [0xa0].as_slice();
        assert_eq!(bits.read(&mut input, 3), Ok(Some(5)));
        assert_eq!(bits.read(&mut input, 8), Ok(None));
        assert!(FrameCrc::default().into_footer(&mut bits).is_err());
        let mut rest = [0x20].as_slice();
        let mut count = 0;
        assert_eq!(bits.read(&mut rest, 5), Err(()));
        assert_eq!(bits.unary(&mut rest, &mut count, 31), Err(()));
        assert_eq!(rest, [0x20]);
        assert_eq!(bits.read(&mut rest, 8), Ok(Some(1)));
        assert_eq!(bits.unary(&mut rest, &mut count, 31), Ok(false));
        assert_eq!(count, 5);
        assert!(FrameCrc::default().into_footer(&mut bits).is_err());
        assert_eq!(bits.read(&mut [0x80].as_slice(), 1), Err(()));
        assert_eq!(bits.unary(&mut [0x80].as_slice(), &mut count, 31), Ok(true));
        assert!(FrameCrc::default().into_footer(&mut bits).is_ok());
    }

    #[test]
    fn rejects_wrong_and_truncated_crc() {
        let body = padded_frame_body();
        let [high, low] = PADDED_CRC;
        for wrong in [[high ^ 1, low], [high, low ^ 1]] {
            let mut footer = padded_footer(&body, body.len()).unwrap();
            assert_eq!(footer.read(&mut wrong.as_slice()), Err(()));
            assert_eq!(footer.read(&mut PADDED_CRC.as_slice()), Err(()));
            assert_eq!(footer.finish_at_eof(), Err(()));
        }
        for len in 0..PADDED_CRC.len() {
            let mut footer = padded_footer(&body, body.len()).unwrap();
            assert_eq!(footer.read(&mut &PADDED_CRC[..len]), Ok(false));
            assert_eq!(footer.finish_at_eof(), Err(()));
        }
    }
}
