//! Bounded reconstruction in the coded subframe domain (RFC 9639 §9.2).

const MAX_ORDER: usize = 32;

pub(crate) struct Predictor {
    history: [i64; MAX_ORDER],
    coefficients: [i16; MAX_ORDER],
    order: usize,
    shift: u8,
    sample_bits: u8,
}

impl Predictor {
    pub(crate) fn fixed(order: u8, warmup: &[i64], sample_bits: u8) -> Result<Self, ()> {
        let coefficients: &[i16] = match order {
            0 => &[],
            1 => &[1],
            2 => &[2, -1],
            3 => &[3, -3, 1],
            4 => &[4, -6, 4, -1],
            _ => return Err(()),
        };
        Self::new(coefficients, 0, warmup, sample_bits)
    }

    /// The bitstream parser supplies sign-extended coefficients in encoded order
    /// (most recent sample first). Warmup samples arrive oldest first.
    pub(crate) fn lpc(
        coefficients: &[i16],
        shift: u8,
        warmup: &[i64],
        sample_bits: u8,
    ) -> Result<Self, ()> {
        if coefficients.is_empty() || shift > 15 {
            return Err(());
        }
        Self::new(coefficients, shift, warmup, sample_bits)
    }

    fn new(coefficients: &[i16], shift: u8, warmup: &[i64], sample_bits: u8) -> Result<Self, ()> {
        let order = coefficients.len();
        if order > MAX_ORDER
            || warmup.len() != order
            || !fits_signed(0, sample_bits)
            || warmup
                .iter()
                .any(|&sample| !fits_signed(sample, sample_bits))
        {
            return Err(());
        }
        let mut predictor = Self {
            history: [0; MAX_ORDER],
            coefficients: [0; MAX_ORDER],
            order,
            shift,
            sample_bits,
        };
        predictor.history[..order].copy_from_slice(warmup);
        predictor.coefficients[..order].copy_from_slice(coefficients);
        Ok(predictor)
    }

    /// Failed range checks leave history unchanged; a decoder must refuse the subframe.
    pub(crate) fn restore(&mut self, residual: i32) -> Result<i64, ()> {
        let history = &mut self.history[..self.order];
        // Even with 32 signed 16-bit coefficients and signed 33-bit samples,
        // the sum's magnitude is at most 2^52 before shifting and adding the residual.
        let prediction: i64 = self.coefficients[..self.order]
            .iter()
            .zip(history.iter().rev())
            .map(|(&coefficient, &sample)| i64::from(coefficient) * sample)
            .sum();
        let sample = (prediction >> self.shift) + i64::from(residual);
        if residual == i32::MIN || !fits_signed(sample, self.sample_bits) {
            return Err(());
        }
        if self.order != 0 {
            history.copy_within(1.., 0);
            history[self.order - 1] = sample;
        }
        Ok(sample)
    }
}

pub(crate) fn fits_signed(sample: i64, bits: u8) -> bool {
    (1..=33).contains(&bits) && {
        let half = 1_i64 << (bits - 1);
        (-half..half).contains(&sample)
    }
}

/// Restore wasted low bits after prediction, never in the predictor history.
pub(crate) fn restore_wasted(sample: i64, sample_bits: u8, wasted: u8) -> Result<i64, ()> {
    if !fits_signed(sample, sample_bits) || wasted > 33 - sample_bits {
        return Err(());
    }
    Ok(sample << wasted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_fixed_orders_use_coded_history() {
        for (order, warmup, expected) in [
            (0, &[][..], -1),
            (1, &[2][..], 1),
            (2, &[2, 5][..], 7),
            (3, &[2, 5, 10][..], 16),
            (4, &[2, 5, 10, 17][..], 25),
        ] {
            let mut predictor = Predictor::fixed(order, warmup, 8).unwrap();
            assert_eq!(predictor.restore(-1), Ok(expected));
            assert_eq!(restore_wasted(expected, 8, 3), Ok(expected * 8));
        }
        let mut predictor = Predictor::fixed(2, &[2, 5], 8).unwrap();
        assert_eq!(predictor.restore(-1), Ok(7));
        assert_eq!(predictor.restore(0), Ok(9));
    }

    #[test]
    fn lpc_signed_shift_order_and_wide_accumulator() {
        let mut predictor = Predictor::lpc(&[3, -2], 1, &[5, 1], 8).unwrap();
        assert_eq!(predictor.restore(1), Ok(-3)); // (-7 >> 1) + 1
        assert_eq!(predictor.restore(0), Ok(-6));

        let high = (1_i64 << 32) - 1;
        let mut coefficients = [16383; 32];
        coefficients[16..].fill(-16383);
        let mut predictor = Predictor::lpc(&coefficients, 0, &[high; 32], 33).unwrap();
        assert_eq!(predictor.restore(7), Ok(7)); // Partial sums exceed i32 before cancellation.
    }

    #[test]
    fn rejects_invalid_parameters_and_out_of_range_samples() {
        assert!(Predictor::fixed(5, &[0; 5], 8).is_err());
        assert!(Predictor::fixed(1, &[], 8).is_err());
        assert!(Predictor::fixed(1, &[128], 8).is_err());
        assert!(Predictor::fixed(0, &[], 0).is_err());
        assert!(Predictor::lpc(&[], 0, &[], 8).is_err());
        assert!(Predictor::lpc(&[0; 33], 0, &[0; 33], 8).is_err());
        assert!(Predictor::lpc(&[1], 16, &[0], 8).is_err());

        let mut predictor = Predictor::fixed(1, &[127], 8).unwrap();
        assert_eq!(predictor.restore(1), Err(()));
        assert_eq!(predictor.restore(0), Ok(127));
        let mut predictor = Predictor::fixed(1, &[-128], 8).unwrap();
        assert_eq!(predictor.restore(-1), Err(()));
        let mut predictor = Predictor::fixed(1, &[0], 33).unwrap();
        assert_eq!(predictor.restore(i32::MIN), Err(()));
        assert_eq!(restore_wasted(-1, 1, 32), Ok(-(1_i64 << 32)));
        assert_eq!(restore_wasted(-1, 1, 33), Err(()));
        assert_eq!(restore_wasted(1, 1, 0), Err(()));
        assert_eq!(restore_wasted(0, 34, 0), Err(()));
    }
}
