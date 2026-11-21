//! Read one coded channel's samples without buffering a FLAC block.

use crate::flac_predictor::{restore_wasted, Predictor};
use crate::flac_residual::{Residual, ResidualReader};
use crate::flac_subframe::{BitInput, SubframeHeader, SubframeKind};

#[derive(Clone, Copy)]
enum State {
    Constant,
    Repeat(i64),
    Verbatim,
    Warmup,
    Predict,
    Rejected,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Samples {
    Sample(i64),
    NeedInput,
    End,
}

pub(crate) struct SubframeSamples {
    header: SubframeHeader,
    block_size: u16,
    emitted: u16,
    state: State,
    order: usize,
    warmup: [i64; 4],
    predictor: Option<Predictor>,
    residuals: ResidualReader,
}

impl SubframeSamples {
    pub(crate) fn new(header: SubframeHeader, block_size: u16) -> Result<Self, ()> {
        let (state, order) = match header.kind {
            SubframeKind::Constant => (State::Constant, 0),
            SubframeKind::Verbatim => (State::Verbatim, 0),
            SubframeKind::Fixed(0) => (State::Predict, 0),
            SubframeKind::Fixed(order @ 1..=4) => (State::Warmup, order),
            _ => return Err(()),
        };
        if u16::from(order) >= block_size {
            return Err(());
        }
        restore_wasted(0, header.sample_bits, header.wasted)?;
        let predictor = if matches!(state, State::Predict) {
            Some(Predictor::fixed(0, &[], header.sample_bits)?)
        } else {
            None
        };
        Ok(Self {
            header,
            block_size,
            emitted: 0,
            state,
            order: usize::from(order),
            warmup: [0; 4],
            predictor,
            residuals: ResidualReader::new(block_size, order),
        })
    }

    /// `Sample` may be available without new input (constant and zero-width escape).
    pub(crate) fn read(&mut self, bits: &mut BitInput, input: &mut &[u8]) -> Result<Samples, ()> {
        let result = self.read_next(bits, input);
        if result.is_err() {
            self.state = State::Rejected;
        }
        result
    }

    fn read_next(&mut self, bits: &mut BitInput, input: &mut &[u8]) -> Result<Samples, ()> {
        if self.emitted == self.block_size {
            return Ok(Samples::End);
        }
        let width = self.header.sample_bits;
        macro_rules! field {
            ($width:expr) => {
                match bits.read(input, $width)? {
                    Some(value) => value,
                    None => return Ok(Samples::NeedInput),
                }
            };
        }
        let sample = match self.state {
            State::Constant => {
                let value = signed(field!(width), width);
                self.state = State::Repeat(value);
                value
            }
            State::Repeat(value) => value,
            State::Verbatim => signed(field!(width), width),
            State::Warmup => {
                let value = signed(field!(width), width);
                let index = usize::from(self.emitted);
                self.warmup[index] = value;
                if index + 1 == self.order {
                    let SubframeKind::Fixed(order) = self.header.kind else {
                        return Err(());
                    };
                    self.predictor =
                        Some(Predictor::fixed(order, &self.warmup[..self.order], width)?);
                    self.state = State::Predict;
                }
                value
            }
            State::Predict => match self.residuals.read(bits, input)? {
                Residual::Sample(value) => self.predictor.as_mut().ok_or(())?.restore(value)?,
                Residual::NeedInput => return Ok(Samples::NeedInput),
                Residual::End => return Err(()),
            },
            State::Rejected => return Err(()),
        };
        let sample = restore_wasted(sample, width, self.header.wasted)?;
        self.emitted += 1;
        Ok(Samples::Sample(sample))
    }
}

fn signed(word: u64, width: u8) -> i64 {
    let shift = 64 - width;
    ((word << shift) as i64) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoder(kind: SubframeKind, sample_bits: u8, wasted: u8, block: u16) -> SubframeSamples {
        SubframeSamples::new(
            SubframeHeader {
                kind,
                wasted,
                sample_bits,
            },
            block,
        )
        .unwrap()
    }

    #[test]
    fn fixed_wasted_bits_do_not_feed_prediction_and_zero_escape_drains() {
        let mut reader = decoder(SubframeKind::Fixed(2), 7, 1, 5);
        let mut bits = BitInput::default();
        let mut input = [0x04, 0x14, 0x0f, 0x05, 0xff].as_slice();
        for expected in [4, 10, 16, 22, 28] {
            assert_eq!(
                reader.read(&mut bits, &mut input),
                Ok(Samples::Sample(expected))
            );
        }
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Samples::End));
        assert_eq!(bits.read(&mut input, 3), Ok(Some(5)));
        assert_eq!(input, [0xff]);
    }

    #[test]
    fn constant_and_verbatim_33_bit_samples_preserve_the_tail() {
        for (kind, block) in [(SubframeKind::Constant, 2), (SubframeKind::Verbatim, 1)] {
            let mut reader = decoder(kind, 33, 0, block);
            let mut bits = BitInput::default();
            let mut input = [0x80, 0, 0, 0, 0x7f, 0xff].as_slice();
            for _ in 0..block {
                assert_eq!(
                    reader.read(&mut bits, &mut input),
                    Ok(Samples::Sample(-(1_i64 << 32)))
                );
            }
            assert_eq!(reader.read(&mut bits, &mut input), Ok(Samples::End));
            assert_eq!(bits.read(&mut input, 7), Ok(Some(127)));
            assert_eq!(input, [0xff]);
        }
    }

    #[test]
    fn out_of_range_fixed_sample_refuses_stickily() {
        let mut reader = decoder(SubframeKind::Fixed(1), 1, 0, 2);
        let mut bits = BitInput::default();
        let mut input = [0x00, 0x04].as_slice(); // Warmup 0, then Rice-coded residual +1.
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Samples::Sample(0)));
        assert_eq!(reader.read(&mut bits, &mut input), Err(()));
        assert_eq!(reader.read(&mut bits, &mut [].as_slice()), Err(()));
    }
}
