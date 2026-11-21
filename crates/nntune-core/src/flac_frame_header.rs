//! Native FLAC frame header at an exact audio boundary (RFC 9639 §9.1).

use crate::flac_preamble::StreamInfo;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Position {
    Frame(u32),
    Sample(u64),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FrameHeader {
    pub(crate) position: Position,
    pub(crate) block_size: u16,
    pub(crate) channel_assignment: u8,
    pub(crate) bits_per_sample: u8,
    pub(crate) sample_rate: u32,
    pub(crate) len: usize,
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |crc, &byte| {
        let mut crc = crc ^ byte;
        for _ in 0..8 {
            crc = (crc << 1) ^ if crc & 0x80 != 0 { 0x07 } else { 0 };
        }
        crc
    })
}

impl FrameHeader {
    /// `None` needs more bytes (a header is at most 16 bytes). This never
    /// scans for another sync marker or consumes the following subframes.
    /// Continuity and short-final-block checks belong to the frame stream.
    pub(crate) fn parse(bytes: &[u8], info: &StreamInfo) -> Result<Option<Self>, ()> {
        let Some(&[sync, strategy, codes, format, first, ..]) = bytes.get(..5) else {
            return Ok(None);
        };
        if sync != 0xff || !matches!(strategy, 0xf8 | 0xf9) || format & 1 != 0 {
            return Err(());
        }
        let channel_assignment = format >> 4;
        if channel_assignment > 10 {
            return Err(());
        }
        let bits_per_sample = match (format >> 1) & 7 {
            0 => info.bits_per_sample,
            1 => 8,
            2 => 12,
            3 => return Err(()),
            4 => 16,
            5 => 20,
            6 => 24,
            _ => 32,
        };
        let width = match first.leading_ones() as usize {
            0 => 1,
            n @ 2..=6 => n,
            7 if strategy == 0xf9 => 7,
            _ => return Err(()),
        };
        let block_code = codes >> 4;
        let rate_code = codes & 15;
        let block_len = match block_code {
            0 => return Err(()),
            6 => 1,
            7 => 2,
            _ => 0,
        };
        let rate_len = match rate_code {
            12 => 1,
            13 | 14 => 2,
            15 => return Err(()),
            _ => 0,
        };
        let len = 4 + width + block_len + rate_len + 1;
        let Some(header) = bytes.get(..len) else {
            return Ok(None);
        };
        if crc8(&header[..len - 1]) != header[len - 1] {
            return Err(());
        }
        let mut number = u64::from(first & (0x7f >> if width == 1 { 0 } else { width }));
        for &next in &header[5..4 + width] {
            if next & 0xc0 != 0x80 {
                return Err(());
            }
            number = (number << 6) | u64::from(next & 0x3f);
        }
        const MIN_NUMBER_BITS: [u8; 7] = [0, 7, 11, 16, 21, 26, 31];
        if width > 1 && number < (1 << MIN_NUMBER_BITS[width - 1]) {
            return Err(());
        }
        let extra = &header[4 + width..len - 1];
        let block_size = match block_code {
            1 => 192,
            2..=5 => 576 << (block_code - 2),
            6 => u16::from(extra[0]) + 1,
            7 => u16::from_be_bytes([extra[0], extra[1]])
                .checked_add(1)
                .ok_or(())?,
            8..=15 => 256 << (block_code - 8),
            _ => return Err(()),
        };
        let rate = &extra[block_len..];
        let sample_rate = match rate_code {
            0 => info.sample_rate,
            1..=11 => [
                88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
            ][usize::from(rate_code - 1)],
            12 => u32::from(rate[0]) * 1000,
            13 => u32::from(u16::from_be_bytes([rate[0], rate[1]])),
            14 => u32::from(u16::from_be_bytes([rate[0], rate[1]])) * 10,
            _ => return Err(()),
        };
        if block_size > info.max_block || sample_rate == 0 {
            return Err(());
        }
        let position = if strategy == 0xf8 {
            Position::Frame(u32::try_from(number).map_err(|_| ())?)
        } else {
            Position::Sample(number)
        };
        Ok(Some(Self {
            position,
            block_size,
            channel_assignment,
            bits_per_sample,
            sample_rate,
            len,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXED: [u8; 6] = [0xff, 0xf8, 0x80, 0, 0, 0x81];
    const VARIABLE: [u8; 16] = [
        0xff, 0xf9, 0x7e, 0xa8, 0xfe, 0x82, 0xa3, 0xb4, 0xa5, 0xb6, 0xa7, 0, 15, 0x11, 0x3a, 0xa5,
    ];
    const INFO: StreamInfo = StreamInfo {
        min_block: 16,
        max_block: 4096,
        sample_rate: 48000,
        channels: 1,
        bits_per_sample: 24,
        total_samples: 0,
        pcm_md5: [0; 16],
    };

    #[test]
    fn exact_header_boundary_and_numbering() {
        for (header, position, size, assignment, format) in [
            (FIXED.as_slice(), Position::Frame(0), 256, 0, (24, 48000)),
            (
                VARIABLE.as_slice(),
                Position::Sample(0xa3d25da7),
                16,
                10,
                (16, 44100),
            ),
        ] {
            for i in 0..header.len() {
                assert_eq!(FrameHeader::parse(&header[..i], &INFO), Ok(None));
            }
            let with_tail = [header, &[0x80, 0xff, 0xf8]].concat();
            let parsed = FrameHeader::parse(&with_tail, &INFO).unwrap().unwrap();
            assert_eq!(parsed.len, header.len());
            assert_eq!(parsed.position, position);
            assert_eq!(parsed.block_size, size);
            assert_eq!(parsed.channel_assignment, assignment);
            assert_eq!((parsed.bits_per_sample, parsed.sample_rate), format);
        }
    }

    #[test]
    fn malformed_headers_are_not_resynchronized() {
        let mut corrupt = FIXED;
        corrupt[5] ^= 1;
        assert_eq!(FrameHeader::parse(&corrupt, &INFO), Err(()));
        for mut bad in [
            vec![0, 0xf8, 0x89, 8, 0],                // wrong sync
            vec![0xff, 0xf8, 0x89, 9, 0],             // reserved bit
            vec![0xff, 0xf8, 0x89, 6, 0],             // reserved depth
            vec![0xff, 0xf8, 0x8c, 8, 0, 0],          // zero audio rate
            vec![0xff, 0xf8, 0x89, 0xb8, 0],          // reserved channels
            vec![0xff, 0xf8, 0x89, 8, 0xc0, 0x80],    // overlong zero
            vec![0xff, 0xf8, 0x89, 8, 0xc2, 0x40],    // bad continuation
            vec![0xff, 0xf8, 0x79, 8, 0, 0xff, 0xff], // 65536 samples
        ] {
            bad.push(crc8(&bad));
            bad.extend_from_slice(&FIXED);
            assert_eq!(FrameHeader::parse(&bad, &INFO), Err(()));
        }
    }
}
