//! Unmounted FLAC frame order and sample-count checks (RFC 9639 §§8.2, 9.1).

use crate::flac_frame::FrameReader;
use crate::flac_frame_header::Position;
use crate::flac_preamble::StreamInfo;

enum Next {
    First,
    Fixed(u32),
    Variable,
    EndOnly,
}

pub(crate) struct FrameContinuity<'a> {
    info: &'a StreamInfo,
    samples: u64,
    next: Option<Next>,
}

impl<'a> FrameContinuity<'a> {
    pub(crate) fn new(info: &'a StreamInfo) -> Self {
        Self {
            info,
            samples: 0,
            next: Some(Next::First),
        }
    }

    /// Advance only after the reader accepts the full frame including its CRC.
    /// The caller starts each reader at the exact next boundary using the same STREAMINFO.
    pub(crate) fn accept(&mut self, frame: FrameReader) -> Result<(), ()> {
        let next = self.next.take().ok_or(())?;
        let header = frame.finish()?;
        let block = header.block_size;
        if block == 0 || block > self.info.max_block {
            return Err(());
        }
        let next = match (next, header.position) {
            (Next::First, Position::Frame(0)) => self.after_fixed(0, block)?,
            (Next::Fixed(expected), Position::Frame(n)) if n == expected => {
                self.after_fixed(n, block)?
            }
            (Next::First | Next::Variable, Position::Sample(n)) if n == self.samples => {
                if block < self.info.min_block {
                    Next::EndOnly
                } else {
                    Next::Variable
                }
            }
            _ => return Err(()),
        };
        let samples = self.samples.checked_add(u64::from(block)).ok_or(())?;
        if self.info.total_samples != 0 && samples > self.info.total_samples {
            return Err(());
        }
        self.next = Some(next);
        self.samples = samples;
        Ok(())
    }

    fn after_fixed(&self, number: u32, block: u16) -> Result<Next, ()> {
        if self.info.min_block != self.info.max_block {
            return Err(());
        }
        Ok(if block < self.info.max_block {
            Next::EndOnly
        } else {
            Next::Fixed(number.checked_add(1).ok_or(())?)
        })
    }

    /// The caller must separately establish exact authenticated EOF with no pending bytes.
    pub(crate) fn finish_at_frame_boundary(self) -> Result<(), ()> {
        if self.next.is_some()
            && self.samples != 0
            && (self.info.total_samples == 0 || self.samples == self.info.total_samples)
        {
            Ok(())
        } else {
            Err(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // CRCs pinned with polynomial long division, independent of the frame reader.
    const FIXED: &[u8] = &[255, 248, 105, 2, 0, 15, 183, 0, 42, 122, 221];
    const TAIL: &[u8] = &[255, 248, 105, 2, 1, 2, 129, 0, 42, 157, 29];
    const SKIP: &[u8] = &[255, 248, 105, 2, 2, 15, 157, 0, 42, 120, 37];
    const SWITCH: &[u8] = &[255, 249, 105, 2, 16, 15, 130, 0, 42, 238, 220];
    const VARIABLE: &[u8] = &[255, 249, 105, 2, 0, 23, 157, 0, 42, 136, 213];
    const NEXT: &[u8] = &[255, 249, 105, 2, 24, 15, 42, 0, 42, 229, 60];
    const GAP: &[u8] = &[255, 249, 105, 2, 25, 15, 63, 0, 42, 228, 64];
    const SHORT: &[u8] = &[255, 249, 105, 2, 24, 2, 9, 0, 42, 3, 128];
    const AFTER_SHORT: &[u8] = &[255, 249, 105, 2, 27, 15, 21, 0, 42, 230, 184];
    const INFO: StreamInfo = StreamInfo {
        min_block: 16,
        max_block: 16,
        total_samples: 19,
        sample_rate: 44_100,
        channels: 1,
        bits_per_sample: 8,
        pcm_md5: [0; 16],
    };

    fn check(info: &StreamInfo, inputs: &[&[u8]]) -> Result<(), ()> {
        let mut stream = FrameContinuity::new(info);
        for mut wire in inputs.iter().copied() {
            let mut frame = FrameReader::start(&mut wire, info)?.unwrap();
            assert_eq!(frame.read(&mut wire), Ok(true));
            assert!(wire.is_empty());
            stream.accept(frame)?;
        }
        stream.finish_at_frame_boundary()
    }

    #[test]
    fn fixed_continuity_requires_consecutive_frames_and_exact_total() {
        assert_eq!(check(&INFO, &[FIXED, TAIL]), Ok(()));
        assert_eq!(check(&INFO, &[FIXED]), Err(()));
        assert_eq!(
            check(
                &StreamInfo {
                    total_samples: 18,
                    ..INFO
                },
                &[FIXED, TAIL]
            ),
            Err(())
        );
        let unknown = StreamInfo {
            total_samples: 0,
            ..INFO
        };
        for invalid in [
            &[][..],
            &[TAIL][..],
            &[FIXED, FIXED],
            &[FIXED, SKIP],
            &[FIXED, SWITCH],
            &[FIXED, TAIL, SKIP],
        ] {
            assert_eq!(check(&unknown, invalid), Err(()));
        }
    }

    #[test]
    fn variable_continuity_rejects_gaps_and_frames_after_a_short_tail() {
        let info = StreamInfo {
            max_block: 24,
            total_samples: 0,
            ..INFO
        };
        assert_eq!(check(&info, &[VARIABLE, NEXT]), Ok(()));
        assert_eq!(check(&info, &[VARIABLE, SHORT]), Ok(()));
        assert_eq!(check(&info, &[VARIABLE, GAP]), Err(()));
        assert_eq!(check(&info, &[VARIABLE, SHORT, AFTER_SHORT]), Err(()));
    }

    #[test]
    fn unfinished_or_corrupt_frames_poison_continuity() {
        let mut damaged = FIXED.to_vec();
        damaged[10] ^= 1;
        for (mut wire, result) in [(&FIXED[..10], Ok(false)), (damaged.as_slice(), Err(()))] {
            let mut stream = FrameContinuity::new(&INFO);
            let mut frame = FrameReader::start(&mut wire, &INFO).unwrap().unwrap();
            assert_eq!(frame.read(&mut wire), result);
            assert_eq!(stream.accept(frame), Err(()));
            let mut valid = FIXED;
            let mut frame = FrameReader::start(&mut valid, &INFO).unwrap().unwrap();
            assert_eq!(frame.read(&mut valid), Ok(true));
            assert_eq!(stream.accept(frame), Err(()));
            assert_eq!(stream.finish_at_frame_boundary(), Err(()));
        }
    }
}
