//! Reading one sample out of a [`crate::compare::quality::Signal`] plane,
//! shared by every metric so the 8-bit/16-bit split lives in exactly one
//! place.

use crate::compare::quality::Signal;

/// Bytes per sample for a given bit depth. `None` for anything this crate's
/// metrics do not support (`0` or `> 16` bits).
#[must_use]
pub fn bytes_per_sample(depth: u8) -> Option<usize> {
    match depth {
        1..=8 => Some(1),
        9..=16 => Some(2),
        _ => None,
    }
}

/// The maximum representable value at `depth` bits — PSNR's `L`.
#[must_use]
pub fn max_value(depth: u8) -> f64 {
    if depth == 0 {
        0.0
    } else {
        f64::from((1_u32 << depth.min(31)) - 1)
    }
}

/// One sample at `(x, y)` in `plane`, or `None` if it falls outside the
/// plane's declared bounds (including a truncated buffer — this never
/// indexes past what `plane` actually contains).
#[must_use]
pub fn sample_at(plane: &[u8], stride: usize, x: u32, y: u32, depth: u8) -> Option<u32> {
    let bps = bytes_per_sample(depth)?;
    let row_start = usize::try_from(y).ok()?.checked_mul(stride)?;
    let col_offset = usize::try_from(x).ok()?.checked_mul(bps)?;
    let offset = row_start.checked_add(col_offset)?;
    let bytes = plane.get(offset..offset.checked_add(bps)?)?;
    match bps {
        1 => bytes.first().map(|b| u32::from(*b)),
        2 => {
            let lo = *bytes.first()?;
            let hi = *bytes.get(1)?;
            Some(u32::from(u16::from_le_bytes([lo, hi])))
        }
        _ => None,
    }
}

/// Whether `a` and `b` share the geometry every metric needs to compare
/// them sample-for-sample.
#[must_use]
pub fn geometry_matches(a: &Signal<'_>, b: &Signal<'_>) -> bool {
    a.width == b.width && a.height == b.height && a.depth == b.depth
}

#[cfg(test)]
mod tests {
    use super::{bytes_per_sample, max_value, sample_at};

    #[test]
    fn bytes_per_sample_splits_at_eight_bits() {
        assert_eq!(bytes_per_sample(8), Some(1));
        assert_eq!(bytes_per_sample(9), Some(2));
        assert_eq!(bytes_per_sample(16), Some(2));
        assert_eq!(bytes_per_sample(0), None);
        assert_eq!(bytes_per_sample(17), None);
    }

    #[test]
    fn max_value_matches_the_familiar_numbers() {
        assert!((max_value(8) - 255.0).abs() < f64::EPSILON);
        assert!((max_value(10) - 1023.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sample_at_reads_8_bit_row_major() {
        // 2x2, stride 2: [10, 20, 30, 40]
        let plane = [10_u8, 20, 30, 40];
        assert_eq!(sample_at(&plane, 2, 0, 0, 8), Some(10));
        assert_eq!(sample_at(&plane, 2, 1, 0, 8), Some(20));
        assert_eq!(sample_at(&plane, 2, 0, 1, 8), Some(30));
        assert_eq!(sample_at(&plane, 2, 1, 1, 8), Some(40));
    }

    #[test]
    fn sample_at_reads_16_bit_little_endian() {
        // one row, two 16-bit samples: 0x0100 (256) then 0x0002 (2)
        let plane = [0x00_u8, 0x01, 0x02, 0x00];
        assert_eq!(sample_at(&plane, 4, 0, 0, 10), Some(256));
        assert_eq!(sample_at(&plane, 4, 1, 0, 10), Some(2));
    }

    #[test]
    fn sample_at_refuses_out_of_bounds_reads() {
        let plane = [1_u8, 2, 3, 4];
        assert_eq!(sample_at(&plane, 2, 5, 5, 8), None);
        assert_eq!(sample_at(&plane, 2, 0, 0, 33), None);
    }
}
