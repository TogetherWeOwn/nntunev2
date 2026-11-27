//! Streaming FLAC predictor residuals (RFC 9639 §9.2.7).

use crate::flac_subframe::BitInput;

const MAX_FOLDED: u32 = u32::MAX - 1; // -2^31 is not a valid residual.

#[derive(Clone, Copy)]
enum State {
    Header(u16, u8),
    Parameter,
    RawWidth,
    Raw(u8),
    Unary(u8, u32),
    Remainder(u8, u32),
    End,
    Rejected,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Residual {
    Sample(i32),
    NeedInput,
    End,
}

pub(crate) struct ResidualReader {
    state: State,
    parameter_bits: u8,
    partitions_left: u16,
    partition_size: u16,
    samples_left: u16,
    max_folded: u32,
}

impl ResidualReader {
    pub(crate) fn new(block_size: u16, predictor_order: u8, max_folded: u32) -> Self {
        Self {
            state: State::Header(block_size, predictor_order),
            parameter_bits: 0,
            partitions_left: 0,
            partition_size: 0,
            samples_left: 0,
            max_folded: max_folded.min(MAX_FOLDED),
        }
    }

    pub(crate) fn read(&mut self, bits: &mut BitInput, input: &mut &[u8]) -> Result<Residual, ()> {
        let result = self.read_next(bits, input);
        if result.is_err() {
            self.state = State::Rejected;
        }
        result
    }

    fn read_next(&mut self, bits: &mut BitInput, input: &mut &[u8]) -> Result<Residual, ()> {
        macro_rules! field {
            ($width:expr) => {
                match bits.read(input, $width)? {
                    Some(value) => value,
                    None => return Ok(Residual::NeedInput),
                }
            };
        }
        loop {
            self.state = match self.state {
                State::Header(block_size, order) => {
                    let header = field!(6);
                    self.parameter_bits = match header >> 4 {
                        0 => 4,
                        1 => 5,
                        _ => return Err(()),
                    };
                    self.partitions_left = 1 << (header & 15);
                    self.partition_size = block_size / self.partitions_left;
                    let order = u16::from(order);
                    if block_size & (self.partitions_left - 1) != 0 || self.partition_size <= order
                    {
                        return Err(());
                    }
                    self.samples_left = self.partition_size - order;
                    State::Parameter
                }
                State::Parameter => {
                    let parameter = field!(self.parameter_bits) as u8;
                    if parameter == (1 << self.parameter_bits) - 1 {
                        State::RawWidth
                    } else {
                        State::Unary(parameter, 0)
                    }
                }
                State::RawWidth => State::Raw(field!(5) as u8),
                State::Raw(width) => {
                    let value = field!(width) as i32;
                    let shift = 32 - width.max(1); // A zero-width field yields zero.
                    return Ok(self.sample((value << shift) >> shift, State::Raw(width)));
                }
                State::Unary(parameter, mut quotient) => {
                    if !bits.unary(input, &mut quotient, self.max_folded >> parameter)? {
                        self.state = State::Unary(parameter, quotient);
                        return Ok(Residual::NeedInput);
                    }
                    State::Remainder(parameter, quotient)
                }
                State::Remainder(parameter, quotient) => {
                    let folded = (quotient << parameter) | field!(parameter) as u32;
                    if folded > self.max_folded {
                        return Err(());
                    }
                    let sample = (folded >> 1) as i32 ^ -((folded & 1) as i32);
                    return Ok(self.sample(sample, State::Unary(parameter, 0)));
                }
                State::End => return Ok(Residual::End),
                State::Rejected => return Err(()),
            };
        }
    }

    fn sample(&mut self, sample: i32, repeat: State) -> Residual {
        self.samples_left -= 1;
        self.state = if self.samples_left != 0 {
            repeat
        } else {
            self.partitions_left -= 1;
            if self.partitions_left == 0 {
                State::End
            } else {
                self.samples_left = self.partition_size;
                State::Parameter
            }
        };
        Residual::Sample(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rice_and_zero_escape_partitions_preserve_the_exact_boundary() {
        let bytes = [0x04, 0xab, 0x78, 0x28, 0xff];
        for split in 0..=4 {
            let mut bits = BitInput::default();
            let mut reader = ResidualReader::new(6, 1, MAX_FOLDED);
            let (mut input, after_split) = bytes.split_at(split);
            let mut after_split = Some(after_split);
            for expected in [-1, 3, 0, 0, 0] {
                let mut sample = reader.read(&mut bits, &mut input);
                if sample == Ok(Residual::NeedInput) {
                    assert!(input.is_empty());
                    input = after_split.take().unwrap();
                    sample = reader.read(&mut bits, &mut input);
                }
                assert_eq!(sample, Ok(Residual::Sample(expected)));
            }
            assert_eq!(reader.read(&mut bits, &mut input), Ok(Residual::End));
            input = after_split.unwrap_or(input);
            assert_eq!(bits.read(&mut input, 4), Ok(Some(0b1010)));
            assert_eq!(bits.read(&mut input, 2), Ok(Some(0)));
            assert_eq!(input, [0xff]);
        }
    }

    #[test]
    fn rice_quotient_resumes_mid_unary_and_preserves_tail() {
        // Method 0, partition order 0, Rice parameter 1, then six unary zeros.
        let bytes = [0x00, 0x40, 0b10_101101, 0xff];
        let (mut input, mut rest) = bytes.split_at(2);
        let mut bits = BitInput::default();
        let mut reader = ResidualReader::new(1, 0, MAX_FOLDED);
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Residual::NeedInput));
        assert!(input.is_empty());

        // The unary terminator and zero remainder fold to 12 only with the saved quotient.
        assert_eq!(reader.read(&mut bits, &mut rest), Ok(Residual::Sample(6)));
        assert_eq!(reader.read(&mut bits, &mut rest), Ok(Residual::End));
        assert_eq!(bits.read(&mut rest, 6), Ok(Some(0b101101)));
        assert_eq!(rest, [0xff]);
    }

    #[test]
    fn five_bit_parameters_cover_signed_escape_and_folded_limit() {
        let mut bits = BitInput::default();
        let mut input = [0x43, 0xe3, 0x8f].as_slice();
        let mut reader = ResidualReader::new(2, 0, MAX_FOLDED);
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Residual::Sample(-4)));
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Residual::Sample(3)));
        assert_eq!(reader.read(&mut bits, &mut input), Ok(Residual::End));
        for (last, expected) in [(0xf0, Ok(Residual::Sample(i32::MAX))), (0xf8, Err(()))] {
            let mut reader = ResidualReader::new(1, 0, MAX_FOLDED);
            let mut bits = BitInput::default();
            assert_eq!(
                reader.read(&mut bits, &mut [0x43, 0xc3].as_slice()),
                Ok(Residual::NeedInput)
            );
            assert_eq!(
                reader.read(&mut bits, &mut [0xff, 0xff, 0xff, last].as_slice()),
                expected
            );
        }
    }

    #[test]
    fn invalid_geometry_coding_method_and_unary_overflow_are_sticky() {
        for (size, order, mut input) in [
            (5, 0, &[0x04][..]),
            (6, 3, &[0x04]),
            (6, 1, &[0x80]),
            (1, 0, &[0x43, 0xc1]),
        ] {
            let mut reader = ResidualReader::new(size, order, MAX_FOLDED);
            let mut bits = BitInput::default();
            assert!(reader.read(&mut bits, &mut input).is_err());
            assert!(reader.read(&mut bits, &mut [].as_slice()).is_err());
        }
    }
}
