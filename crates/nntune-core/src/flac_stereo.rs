//! FLAC stereo decorrelation for one pair of restored subframe samples.

use crate::flac_predictor::fits_signed;

#[derive(Debug, PartialEq, Eq)]
enum Assignment {
    LeftSide,
    SideRight,
    MidSide,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Stereo {
    assignment: Assignment,
    bits: u8,
}

impl Stereo {
    /// Independent channels are not decorrelated. Only assignments 8–10
    /// use a side channel with one more bit than the frame's sample depth.
    pub(crate) fn new(assignment: u8, bits: u8) -> Result<Self, ()> {
        if !(4..=32).contains(&bits) {
            return Err(());
        }
        let assignment = match assignment {
            8 => Assignment::LeftSide,
            9 => Assignment::SideRight,
            10 => Assignment::MidSide,
            _ => return Err(()),
        };
        Ok(Self { assignment, bits })
    }

    /// Widths before any wasted bits are removed, in subframe order.
    pub(crate) fn coded_widths(&self) -> [u8; 2] {
        match self.assignment {
            Assignment::SideRight => [self.bits + 1, self.bits],
            Assignment::LeftSide | Assignment::MidSide => [self.bits, self.bits + 1],
        }
    }

    /// Input samples already include any restored wasted bits. Return PCM in
    /// left/right order only if both outputs fit the frame's declared depth.
    pub(crate) fn restore(&self, first: i64, second: i64) -> Result<(i32, i32), ()> {
        let [first_bits, second_bits] = self.coded_widths();
        if !fits_signed(first, first_bits) || !fits_signed(second, second_bits) {
            return Err(());
        }
        // Inputs have at most 33 signed bits, including before cancellation.
        let (left, right) = match self.assignment {
            Assignment::LeftSide => (first, first - second),
            Assignment::SideRight => (first + second, second),
            Assignment::MidSide => {
                // mid = floor((left + right) / 2); the side's low bit
                // carries the parity lost by that floor, even for odd negatives.
                let right = first - (second >> 1);
                (right + second, right)
            }
        };
        if !fits_signed(left, self.bits) || !fits_signed(right, self.bits) {
            return Err(());
        }
        Ok((left as i32, right as i32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check_pair(bits: u8, left: i32, right: i32) {
        let (l, r) = (i64::from(left), i64::from(right));
        let side = l - r;
        for (assignment, inputs, widths) in [
            (8, (l, side), [bits, bits + 1]),
            (9, (side, r), [bits + 1, bits]),
            (10, ((l + r).div_euclid(2), side), [bits, bits + 1]),
        ] {
            let stereo = Stereo::new(assignment, bits).unwrap();
            assert_eq!(stereo.coded_widths(), widths);
            assert_eq!(stereo.restore(inputs.0, inputs.1), Ok((left, right)));
        }
    }

    #[test]
    fn restores_all_small_pairs_and_33_bit_side_extrema() {
        for left in -8..=7 {
            for right in -8..=7 {
                check_pair(4, left, right);
            }
        }
        for (left, right) in [
            (i32::MIN, i32::MAX),
            (i32::MAX, i32::MIN),
            (i32::MIN, i32::MIN),
            (i32::MAX, i32::MAX),
        ] {
            check_pair(32, left, right);
        }
    }

    #[test]
    fn refuses_invalid_assignments_depths_and_samples() {
        for assignment in (0..8).chain(11..=u8::MAX) {
            assert_eq!(Stereo::new(assignment, 16), Err(()));
        }
        for bits in [0, 3, 33, u8::MAX] {
            assert_eq!(Stereo::new(8, bits), Err(()));
        }
        for (assignment, first, second) in [
            (8, 8, 1),   // Left input exceeds depth, right would fit.
            (8, -8, 1),  // Right output underflows.
            (9, -16, 0), // Coded side fits, left output underflows.
            (9, 16, -8), // Coded side exceeds its extra bit.
            (10, 7, 1),  // Odd positive side makes left overflow.
            (10, 7, -1), // Odd negative side makes right overflow.
        ] {
            assert_eq!(
                Stereo::new(assignment, 4).unwrap().restore(first, second),
                Err(())
            );
        }
        assert_eq!(Stereo::new(10, 32).unwrap().restore(0, i64::MIN), Err(()));
    }
}
