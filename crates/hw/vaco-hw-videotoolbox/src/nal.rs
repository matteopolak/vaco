//! Splitting Annex-B into NAL units, in exactly the shape `VideoToolbox` wants
//! them: start codes stripped, emulation-prevention bytes left untouched
//! (`CMVideoFormatDescriptionCreateFromH264ParameterSets`'s own doc requires
//! parameter sets to still carry them — this function does not attempt
//! RBSP de-escaping at all, since nothing here needs it).

/// Every NAL unit in `data`, in stream order, with the 3- or 4-byte start
/// code before each one removed and no trailing zero padding included.
///
/// A run of zero bytes immediately preceding a start code belongs to that
/// start code (the common `00 00 00 01` 4-byte form is a 3-byte code with one
/// extra leading zero), not to the previous NAL unit's payload, so it is
/// trimmed from the end of the unit before it rather than the front of the
/// unit after it.
#[must_use]
pub fn split_annex_b(data: &[u8]) -> Vec<&[u8]> {
    let starts: Vec<usize> = data
        .windows(3)
        .enumerate()
        .filter_map(|(i, w)| (w == [0, 0, 1]).then_some(i + 3))
        .collect();

    starts
        .iter()
        .enumerate()
        .filter_map(|(idx, &start)| {
            let raw_end = starts.get(idx + 1).map_or(data.len(), |&next| next - 3);
            let end = start + trailing_nonzero_len(data.get(start..raw_end)?);
            (end > start).then(|| data.get(start..end)).flatten()
        })
        .collect()
}

/// How many bytes of `slice`, counted from the front, remain once trailing
/// zero bytes are dropped.
fn trailing_nonzero_len(slice: &[u8]) -> usize {
    let mut len = slice.len();
    while len > 0 && slice.get(len - 1) == Some(&0) {
        len -= 1;
    }
    len
}

/// The NAL unit type (H.264's low 5 bits of the NAL header byte), or `None`
/// for an empty unit — malformed input, never a panic.
#[must_use]
pub fn nal_unit_type(unit: &[u8]) -> Option<u8> {
    unit.first().map(|&b| b & 0x1F)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_three_byte_start_codes() {
        let data = [0, 0, 1, 0x67, 0xAA, 0, 0, 1, 0x68, 0xBB, 0xCC];
        let units = split_annex_b(&data);
        assert_eq!(units, vec![&[0x67, 0xAA][..], &[0x68, 0xBB, 0xCC][..]]);
    }

    #[test]
    fn treats_four_byte_start_codes_as_three_byte_with_a_leading_zero() {
        let data = [0, 0, 0, 1, 0x67, 0xAA, 0, 0, 0, 1, 0x68, 0xBB];
        let units = split_annex_b(&data);
        assert_eq!(units, vec![&[0x67, 0xAA][..], &[0x68, 0xBB][..]]);
    }

    #[test]
    fn nal_type_reads_the_low_five_bits() {
        assert_eq!(nal_unit_type(&[0x67]), Some(7)); // SPS
        assert_eq!(nal_unit_type(&[0x68]), Some(8)); // PPS
        assert_eq!(nal_unit_type(&[0x65]), Some(5)); // IDR slice
        assert_eq!(nal_unit_type(&[]), None);
    }

    #[test]
    fn empty_input_splits_to_nothing() {
        assert!(split_annex_b(&[]).is_empty());
    }

    #[test]
    fn real_fixture_splits_into_the_expected_four_units() {
        let data = include_bytes!("../tests/fixtures/tiny_baseline_64x64.h264");
        let units = split_annex_b(data);
        let types: Vec<Option<u8>> = units.iter().map(|u| nal_unit_type(u)).collect();
        assert_eq!(types, vec![Some(7), Some(8), Some(6), Some(5)]);
    }
}
