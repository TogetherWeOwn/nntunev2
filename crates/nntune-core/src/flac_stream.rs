//! Structural FLAC stream checks. This does not verify STREAMINFO's PCM MD5,
//! establish authenticated transport EOF, or produce a publication receipt.

use crate::flac_continuity::FrameContinuity;
use crate::flac_frame::FrameReader;
use crate::flac_preamble::FlacPreamble;

#[derive(Default)]
pub(crate) struct FlacStream {
    preamble: FlacPreamble,
    continuity: Option<FrameContinuity>,
    header: [u8; 16],
    filled: usize,
    frame: Option<FrameReader>,
    failed: bool,
}

impl FlacStream {
    pub(crate) fn push(&mut self, input: &[u8]) -> Result<(), ()> {
        if self.failed {
            return Err(());
        }
        self.consume(input).inspect_err(|()| self.failed = true)
    }

    fn consume(&mut self, mut input: &[u8]) -> Result<(), ()> {
        if self.continuity.is_none() {
            let Some((info, audio)) = self.preamble.push(input)? else {
                return Ok(());
            };
            input = audio;
            self.continuity = Some(FrameContinuity::new(&info));
        }
        let continuity = self.continuity.as_mut().ok_or(())?;
        while let Some((&byte, rest)) = input.split_first() {
            if let Some(frame) = &mut self.frame {
                if !frame.read(&mut input)? {
                    return Ok(());
                }
                continuity.accept(self.frame.take().ok_or(())?)?;
            } else {
                // Feed only header bytes into scratch. The body always starts
                // in the original input, even when its header crossed a split.
                *self.header.get_mut(self.filled).ok_or(())? = byte;
                self.filled += 1;
                input = rest;
                let mut header = &self.header[..self.filled];
                if let Some(frame) = FrameReader::start(&mut header, continuity.stream_info())? {
                    if !header.is_empty() {
                        return Err(());
                    }
                    self.frame = Some(frame);
                    self.filled = 0;
                } else if self.filled == self.header.len() {
                    return Err(());
                }
            }
        }
        Ok(())
    }

    /// Call only once the outer reader has confirmed EOF. Partial metadata,
    /// headers, subframes and footers cannot count as a finished stream.
    pub(crate) fn finish_at_eof(self) -> Result<(), ()> {
        if self.failed || self.filled != 0 || self.frame.is_some() {
            return Err(());
        }
        self.continuity.ok_or(())?.finish_at_frame_boundary()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Synthetic 16-sample mono constant frame followed by a three-sample tail.
    const FIRST: &[u8] = &[255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221];
    const TAIL: &[u8] = &[255, 248, 105, 2, 1, 2, 129, 0, 42, 157, 29];

    fn metadata(total: u64) -> Vec<u8> {
        let mut bytes = b"fLaC\0\0\0\x22\0\x10\0\x10\0\0\0\0\0\0".to_vec();
        bytes.extend_from_slice(&((44_100_u64 << 44) | (7 << 36) | total).to_be_bytes());
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&[0x81, 0, 0, 2, 0, 0]);
        bytes
    }

    #[test]
    fn accepts_complete_flac_at_every_stream_split() {
        let wire = [metadata(19).as_slice(), FIRST, TAIL].concat();
        for split in 0..=wire.len() {
            let mut stream = FlacStream::default();
            stream.push(&wire[..split]).unwrap();
            stream.push(&[]).unwrap();
            stream.push(&wire[split..]).unwrap();
            assert_eq!(stream.finish_at_eof(), Ok(()), "split {split}");
        }
        let mut stream = FlacStream::default();
        for byte in wire.chunks(1) {
            stream.push(byte).unwrap();
        }
        assert_eq!(stream.finish_at_eof(), Ok(()));
    }

    #[test]
    fn rejects_truncation_and_pending_bytes_at_eof() {
        let wire = [metadata(19).as_slice(), FIRST, TAIL].concat();
        for end in 0..wire.len() {
            let mut stream = FlacStream::default();
            stream.push(&wire[..end]).unwrap();
            assert_eq!(stream.finish_at_eof(), Err(()), "prefix {end}");
        }
        let unknown_total = [metadata(0).as_slice(), FIRST].concat();
        let mut stream = FlacStream::default();
        stream.push(&unknown_total).unwrap();
        assert_eq!(stream.finish_at_eof(), Ok(()));
        for extra in [b"\0".as_slice(), &FIRST[..7]] {
            let mut stream = FlacStream::default();
            stream.push(&unknown_total).unwrap();
            stream.push(extra).unwrap();
            assert_eq!(stream.finish_at_eof(), Err(()));
        }
    }

    #[test]
    fn poisons_stream_on_bad_footer_or_duplicate_frame() {
        let mut bad_footer = TAIL.to_vec();
        *bad_footer.last_mut().unwrap() ^= 1;
        for invalid in [bad_footer.as_slice(), FIRST] {
            let mut stream = FlacStream::default();
            stream
                .push(&[metadata(0).as_slice(), FIRST].concat())
                .unwrap();
            assert_eq!(stream.push(invalid), Err(()));
            assert_eq!(stream.push(TAIL), Err(()));
            assert_eq!(stream.finish_at_eof(), Err(()));
        }
    }
}
