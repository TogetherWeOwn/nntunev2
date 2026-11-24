//! Incremental native FLAC metadata framing (RFC 9639). Ancillary payloads
//! are opaque here. Reaching audio is not proof of even one valid frame.

#[derive(Clone, Copy)]
pub(crate) struct StreamInfo {
    pub(crate) min_block: u16,
    pub(crate) max_block: u16,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u8,
    pub(crate) bits_per_sample: u8,
    pub(crate) total_samples: u64,
    pub(crate) pcm_md5: [u8; 16],
}

enum State {
    StreamInfo,
    Header,
    Skip { left: usize, last: bool },
    Audio,
    Failed,
}

pub(crate) struct FlacPreamble {
    scratch: [u8; 42],
    filled: usize,
    info: Option<StreamInfo>,
    state: State,
}

impl Default for FlacPreamble {
    fn default() -> Self {
        Self {
            scratch: [0; 42],
            filled: 0,
            info: None,
            state: State::StreamInfo,
        }
    }
}

fn fill(input: &mut &[u8], dst: &mut [u8], filled: &mut usize) -> bool {
    let n = input.len().min(dst.len() - *filled);
    dst[*filled..*filled + n].copy_from_slice(&input[..n]);
    *input = &input[n..];
    *filled += n;
    *filled == dst.len()
}

impl FlacPreamble {
    /// `None` means more metadata is needed; EOF in this state is invalid.
    /// The returned audio tail borrows the original input, including on the
    /// call where the final metadata block ends. No payload is buffered.
    pub(crate) fn push<'a>(
        &mut self,
        bytes: &'a [u8],
    ) -> Result<Option<(StreamInfo, &'a [u8])>, ()> {
        self.consume(bytes)
            .inspect_err(|()| self.state = State::Failed)
    }

    fn consume<'a>(&mut self, mut bytes: &'a [u8]) -> Result<Option<(StreamInfo, &'a [u8])>, ()> {
        loop {
            self.state = match self.state {
                State::StreamInfo => {
                    if !fill(&mut bytes, &mut self.scratch, &mut self.filled) {
                        return Ok(None);
                    }
                    let [b'f', b'L', b'a', b'C', flags @ (0 | 128), 0, 0, 34, info @ ..] =
                        self.scratch
                    else {
                        return Err(());
                    };
                    let min_block = u16::from_be_bytes([info[0], info[1]]);
                    let max_block = u16::from_be_bytes([info[2], info[3]]);
                    let packed = u64::from_be_bytes(info[10..18].try_into().map_err(|_| ())?);
                    let sample_rate = (packed >> 44) as u32;
                    let bits_per_sample = (((packed >> 36) & 31) + 1) as u8;
                    if min_block < 16
                        || max_block < min_block
                        || sample_rate == 0
                        || bits_per_sample < 4
                    {
                        return Err(());
                    }
                    self.info = Some(StreamInfo {
                        min_block,
                        max_block,
                        sample_rate,
                        channels: (((packed >> 41) & 7) + 1) as u8,
                        bits_per_sample,
                        total_samples: packed & ((1 << 36) - 1),
                        pcm_md5: info[18..].try_into().map_err(|_| ())?,
                    });
                    self.filled = 0;
                    if flags == 128 {
                        State::Audio
                    } else {
                        State::Header
                    }
                }
                State::Header => {
                    if !fill(&mut bytes, &mut self.scratch[..4], &mut self.filled) {
                        return Ok(None);
                    }
                    let [flags, a, b, c, ..] = self.scratch;
                    if matches!(flags & 127, 0 | 127) {
                        return Err(());
                    }
                    self.filled = 0;
                    State::Skip {
                        left: u32::from_be_bytes([0, a, b, c]) as usize,
                        last: flags & 128 != 0,
                    }
                }
                State::Skip { left, last } => {
                    if left > bytes.len() {
                        self.state = State::Skip {
                            left: left - bytes.len(),
                            last,
                        };
                        return Ok(None);
                    }
                    bytes = &bytes[left..];
                    if last {
                        State::Audio
                    } else {
                        State::Header
                    }
                }
                State::Audio => return Ok(Some((self.info.ok_or(())?, bytes))),
                State::Failed => return Err(()),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preamble(last: bool) -> Vec<u8> {
        let mut head = b"fLaC\0\0\0\x22\0\x10\x10\0\0\0\0\0\0\0".to_vec();
        head[4] = u8::from(last) << 7;
        let packed = (44_100_u64 << 44) | (1 << 41) | (15 << 36) | 1234;
        head.extend_from_slice(&packed.to_be_bytes());
        head.extend_from_slice(&[0x5a; 16]);
        head
    }

    #[test]
    fn split_metadata_preserves_the_borrowed_audio_tail() {
        let mut bytes = preamble(false);
        bytes.extend_from_slice(&[1, 0, 0, 3, 0, 0, 0, 0x81, 0, 0, 0]);
        let end = bytes.len();
        bytes.extend_from_slice(b"synthetic audio tail");
        for split in 0..end {
            let mut parser = FlacPreamble::default();
            assert!(parser.push(&bytes[..split]).unwrap().is_none());
            let (info, tail) = parser.push(&bytes[split..]).unwrap().unwrap();
            assert!(std::ptr::eq(tail, &bytes[end..]));
            assert_eq!((info.min_block, info.max_block), (16, 4096));
            assert_eq!(
                (info.sample_rate, info.channels, info.bits_per_sample),
                (44_100, 2, 16)
            );
            assert_eq!(info.total_samples, 1234);
            assert_eq!(info.pcm_md5, [0x5a; 16]);
        }
    }

    #[test]
    fn invalid_metadata_poisoning_and_incomplete_boundary() {
        let good = preamble(true);
        for (index, byte) in [(0, 0), (4, 0x81), (7, 33), (9, 15), (10, 0), (21, 0)] {
            let mut input = good.clone();
            input[index] = byte;
            let mut parser = FlacPreamble::default();
            assert!(parser.push(&input).is_err());
            assert!(parser.push(&good).is_err());
        }
        for flags in [0x80, 0xff] {
            let mut input = preamble(false);
            input.extend_from_slice(&[flags, 0, 0, 0]);
            assert!(FlacPreamble::default().push(&input).is_err());
        }
        let mut parser = FlacPreamble::default();
        assert!(parser.push(&good[..41]).unwrap().is_none());
        assert!(parser.push(&[]).unwrap().is_none());
        assert!(parser.push(&good[41..]).unwrap().unwrap().1.is_empty());
        assert_eq!(parser.push(b"next").unwrap().unwrap().1, b"next");
    }
}
