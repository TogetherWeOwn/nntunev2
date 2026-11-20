//! Native FLAC subframe header and MSB-first bit input (RFC 9639 §9.2).

#[derive(Default)]
pub(crate) struct BitInput {
    value: u64,
    available: u8,
}

impl BitInput {
    /// Pull only the bytes needed. On short input, retain the partial field
    /// for the next slice. The maximum field is a 33-bit stereo side sample.
    pub(crate) fn read(&mut self, input: &mut &[u8], width: u8) -> Result<Option<u64>, ()> {
        if width > 33 {
            return Err(());
        }
        while self.available < width {
            let Some((&byte, rest)) = input.split_first() else {
                return Ok(None);
            };
            *input = rest;
            self.value = (self.value << 8) | u64::from(byte);
            self.available += 8;
        }
        self.available -= width;
        let value = self.value >> self.available;
        self.value &= (1 << self.available) - 1;
        Ok(Some(value))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SubframeKind {
    Constant,
    Verbatim,
    Fixed(u8),
    Lpc(u8),
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SubframeHeader {
    pub(crate) kind: SubframeKind,
    pub(crate) wasted: u8,
    pub(crate) sample_bits: u8,
}

pub(crate) struct SubframeHeaderReader {
    channel_bits: u8,
    kind: Option<SubframeKind>,
    wasted: u8,
    done: bool,
}

impl SubframeHeaderReader {
    pub(crate) fn new(channel_bits: u8) -> Result<Self, ()> {
        if !(4..=33).contains(&channel_bits) {
            return Err(());
        }
        Ok(Self {
            channel_bits,
            kind: None,
            wasted: 0,
            done: false,
        })
    }

    /// Parse exactly one header; discard the reader on refusal. The shared
    /// bit input retains the first sample's bits even at an unaligned boundary.
    pub(crate) fn read(
        &mut self,
        bits: &mut BitInput,
        input: &mut &[u8],
    ) -> Result<Option<SubframeHeader>, ()> {
        if self.done {
            return Err(());
        }
        self.done = true;
        let kind = if let Some(kind) = self.kind {
            kind
        } else {
            let Some(header) = bits.read(input, 8)? else {
                self.done = false;
                return Ok(None);
            };
            let kind = match header >> 1 {
                0 => SubframeKind::Constant,
                1 => SubframeKind::Verbatim,
                n @ 8..=12 => SubframeKind::Fixed((n - 8) as u8),
                n @ 32..=63 => SubframeKind::Lpc((n - 31) as u8),
                _ => return Err(()),
            };
            self.kind = Some(kind);
            self.wasted = (header & 1) as u8;
            kind
        };
        if self.wasted != 0 {
            loop {
                if self.wasted >= self.channel_bits {
                    return Err(());
                }
                match bits.read(input, 1)? {
                    Some(1) => break,
                    Some(_) => self.wasted += 1,
                    None => {
                        self.done = false;
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(SubframeHeader {
            kind,
            wasted: self.wasted,
            sample_bits: self.channel_bits - self.wasted,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subframe_type_boundaries() {
        for (header, kind) in [
            (0x00, SubframeKind::Constant),
            (0x02, SubframeKind::Verbatim),
            (0x10, SubframeKind::Fixed(0)),
            (0x18, SubframeKind::Fixed(4)),
            (0x40, SubframeKind::Lpc(1)),
            (0x7e, SubframeKind::Lpc(32)),
        ] {
            let mut bits = BitInput::default();
            let mut reader = SubframeHeaderReader::new(33).unwrap();
            let bytes = [header, 0xff];
            let mut rest = bytes.as_slice();
            assert_eq!(
                reader.read(&mut bits, &mut rest),
                Ok(Some(SubframeHeader {
                    kind,
                    wasted: 0,
                    sample_bits: 33,
                }))
            );
            assert_eq!(rest, [0xff]);
        }
    }

    #[test]
    fn split_wasted_bits_preserve_unaligned_sample_and_tail() {
        let bytes = [0x13, 0x00, 0xd5, 0xaa]; // Fixed order 1, nine wasted bits, then 0x55.
        for split in 0..3 {
            let mut bits = BitInput::default();
            let mut reader = SubframeHeaderReader::new(16).unwrap();
            let mut prefix = &bytes[..split];
            assert_eq!(reader.read(&mut bits, &mut prefix), Ok(None));
            assert!(prefix.is_empty());
            let mut rest = &bytes[split..];
            assert_eq!(
                reader.read(&mut bits, &mut rest).unwrap().unwrap(),
                SubframeHeader {
                    kind: SubframeKind::Fixed(1),
                    wasted: 9,
                    sample_bits: 7
                }
            );
            assert_eq!(bits.read(&mut rest, 7), Ok(Some(0x55)));
            assert_eq!(rest, [0xaa]);
        }

        // Three leading bits, then the same header and sample, then five tail bits.
        let bytes = [0xa2, 0x60, 0x1a, 0xad, 0xaa];
        let mut bits = BitInput::default();
        let mut reader = SubframeHeaderReader::new(16).unwrap();
        let mut prefix = &bytes[..1];
        assert_eq!(bits.read(&mut prefix, 3), Ok(Some(0b101)));
        // Only five bits of the eight-bit header are buffered.
        assert_eq!(reader.read(&mut bits, &mut prefix), Ok(None));
        assert!(prefix.is_empty());
        let mut rest = &bytes[1..];
        assert_eq!(
            reader.read(&mut bits, &mut rest),
            Ok(Some(SubframeHeader {
                kind: SubframeKind::Fixed(1),
                wasted: 9,
                sample_bits: 7,
            }))
        );
        assert_eq!(bits.read(&mut rest, 7), Ok(Some(0x55)));
        assert_eq!(bits.read(&mut rest, 5), Ok(Some(0b01101)));
        assert_eq!(rest, [0xaa]);
    }

    #[test]
    fn reserved_types_padding_and_exhausted_width_are_rejected() {
        for header in [0x04, 0x1a, 0x3e, 0x80] {
            assert!(SubframeHeaderReader::new(4)
                .unwrap()
                .read(&mut BitInput::default(), &mut [header].as_slice())
                .is_err());
        }
        let mut rest = [1, 0x10, 0xff].as_slice(); // Four wasted bits leave no sample bits.
        let mut bits = BitInput::default();
        let mut reader = SubframeHeaderReader::new(4).unwrap();
        assert!(reader.read(&mut bits, &mut rest).is_err());
        assert!(reader.read(&mut bits, &mut rest).is_err());
        assert_eq!(rest, [0xff]);
    }

    #[test]
    fn split_stereo_side_field_retains_following_bits() {
        let mut bits = BitInput::default();
        assert_eq!(bits.read(&mut [0xff; 4].as_slice(), 33), Ok(None));
        let mut rest = [0xd5, 0xaa].as_slice();
        assert_eq!(bits.read(&mut rest, 33), Ok(Some(0x1_ffff_ffff)));
        assert_eq!(bits.read(&mut rest, 7), Ok(Some(0x55)));
        assert_eq!(rest, [0xaa]);
    }
}
