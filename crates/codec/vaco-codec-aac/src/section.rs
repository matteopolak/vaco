//! `section_data()` — ISO/IEC 14496-3 subpart 4 Table 4.52: assigns a
//! Huffman codebook to each run ("section") of consecutive scalefactor
//! bands, per window group.
//!
//! # The trap this module is built to avoid
//!
//! This is the same shape as MP3 Layer III's `region_count` bug this
//! workspace already found and fixed once (an off-by-one in a
//! band-boundary run-length field silently producing plausible garbage
//! instead of an error) — a different codec, the identical failure class.
//! `sect_cb` is **4 bits**, not 5: the 5-bit form only exists under
//! `aacSectionDataResilienceFlag` (an Error-Resilient-profile bitstream
//! feature), which is never set for the plain AAC-LC/ADTS streams this
//! crate reads. `sect_len_incr`'s escape width is **3 bits for
//! `EIGHT_SHORT_SEQUENCE`, 5 bits otherwise** (`sect_esc_val = 2^width -
//! 1`), read repeatedly while it equals the escape value, each read adding
//! `sect_esc_val` to the running length, terminated by a non-escape read
//! that is added once more. Both widths were checked against the primary
//! text directly rather than recalled — see `docs/codec/vaco-codec-aac.md`
//! for the exact line reference.
//!
//! Per-window-group, not per-frame: `EIGHT_SHORT_SEQUENCE`'s `section_data()`
//! runs once per window group (as many as 8, one per raw window, if
//! `scale_factor_grouping` starts a new group at every boundary), each
//! covering its own `0..max_sfb` range independently — the granule-major /
//! subband-major trap in another costume that cost the Layer II MP3 work a
//! full round. [`read_all_groups`] takes `num_window_groups` explicitly
//! rather than assuming 1, for exactly this reason.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

/// `sect_cb` values 13/14/15 are not a Huffman table selection at all —
/// they mark a noise-substitution or intensity-stereo band. Kept as plain
/// `u8` (not an enum) because [`crate::spectral_tables::spectrum_table`]
/// already treats "not 1..=11" as "no table", and scale-factor decoding
/// (the next stage) needs exactly these three magic numbers.
pub(crate) const ZERO_HCB: u8 = 0;
pub(crate) const NOISE_HCB: u8 = 13;
pub(crate) const INTENSITY_HCB2: u8 = 14; // "out of phase"
pub(crate) const INTENSITY_HCB: u8 = 15; // "in phase"

/// Read one window group's `section_data()`: the codebook assigned to each
/// of `max_sfb` scalefactor bands, expanded from the transmitted
/// (codebook, run-length) sections into one entry per band.
///
/// # Errors
///
/// [`Error::InvalidData`] if a section's codebook is the reserved value 12,
/// or if the sections overrun `max_sfb` (both indicate a corrupt or
/// non-AAC-LC-conformant bitstream — not read past, on the same "gate
/// rather than guess" basis as everywhere else in this crate).
/// [`Error::UnexpectedEof`] on truncation.
fn read_one_group(r: &mut BitReader<'_>, max_sfb: u8, is_short: bool) -> Result<Vec<u8>> {
    let sect_len_incr_bits = if is_short { 3 } else { 5 };
    let sect_esc_val = (1u32 << sect_len_incr_bits) - 1;

    let mut bands = Vec::new();
    while bands.len() < usize::from(max_sfb) {
        let sect_cb = r.get(4) as u8;
        if sect_cb == 12 {
            return Err(Error::InvalidData(
                "vaco-codec-aac: section_data sect_cb is the reserved value 12",
            ));
        }
        let mut sect_len = 0u32;
        loop {
            let incr = r.get(sect_len_incr_bits);
            sect_len += incr;
            if incr != sect_esc_val {
                break;
            }
        }
        if sect_len == 0 || bands.len() + sect_len as usize > usize::from(max_sfb) {
            return Err(Error::InvalidData(
                "vaco-codec-aac: section_data section runs past max_sfb",
            ));
        }
        for _ in 0..sect_len {
            bands.push(sect_cb);
        }
    }
    Ok(bands)
}

/// Read `section_data()` for every window group: one `Vec<u8>` (length
/// `max_sfb`, one codebook per band) per group, length `num_window_groups`.
///
/// # Errors
///
/// As [`read_one_group`].
pub(crate) fn read_all_groups(
    r: &mut BitReader<'_>,
    num_window_groups: usize,
    max_sfb: u8,
    is_short: bool,
) -> Result<Vec<Vec<u8>>> {
    let mut groups = Vec::new();
    for _ in 0..num_window_groups {
        groups.push(read_one_group(r, max_sfb, is_short)?);
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic, reason = "test code")]
    use super::read_all_groups;
    use vaco_bitstream::{BitReader, BitWriter};

    #[test]
    fn one_section_covering_every_band() {
        let mut w = BitWriter::new();
        w.put(4, 5); // sect_cb = 5
        w.put(5, 10); // sect_len_incr = 10 (not escape) -> sect_len = 10
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, 1, 10, false).unwrap();
        assert_eq!(groups, vec![vec![5u8; 10]]);
    }

    #[test]
    fn escaped_length_accumulates_across_reads() {
        // Long window: escape value is 31. 31 + 31 + 2 = 64 bands in one
        // section (max_sfb is only 6 bits wide, so 64 is a legal max_sfb).
        let mut w = BitWriter::new();
        w.put(4, 3); // sect_cb
        w.put(5, 31); // escape
        w.put(5, 31); // escape
        w.put(5, 2); // terminate: total 64
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, 1, 64, false).unwrap();
        assert_eq!(groups, vec![vec![3u8; 64]]);
    }

    #[test]
    fn short_window_uses_the_narrower_escape_width() {
        // Short window escape is 7 (3 bits). 7 + 3 = 10 bands.
        let mut w = BitWriter::new();
        w.put(4, 1);
        w.put(3, 7); // escape
        w.put(3, 3); // terminate: total 10
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, 1, 10, true).unwrap();
        assert_eq!(groups, vec![vec![1u8; 10]]);
    }

    #[test]
    fn multiple_sections_in_one_group() {
        let mut w = BitWriter::new();
        w.put(4, 0); // ZERO_HCB, 3 bands
        w.put(5, 3);
        w.put(4, 9); // codebook 9, 2 bands
        w.put(5, 2);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, 1, 5, false).unwrap();
        assert_eq!(groups, vec![vec![0, 0, 0, 9, 9]]);
    }

    #[test]
    fn each_window_group_is_independent() {
        let mut w = BitWriter::new();
        // group 0: one section, cb=2, len=4
        w.put(4, 2);
        w.put(5, 4);
        // group 1: one section, cb=7, len=4
        w.put(4, 7);
        w.put(5, 4);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let groups = read_all_groups(&mut r, 2, 4, false).unwrap();
        assert_eq!(groups, vec![vec![2, 2, 2, 2], vec![7, 7, 7, 7]]);
    }

    #[test]
    fn reserved_codebook_12_is_rejected() {
        let mut w = BitWriter::new();
        w.put(4, 12);
        w.put(5, 1);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(read_all_groups(&mut r, 1, 1, false).is_err());
    }

    #[test]
    fn a_section_running_past_max_sfb_is_rejected() {
        let mut w = BitWriter::new();
        w.put(4, 1);
        w.put(5, 20); // claims 20 bands but max_sfb is 5
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        assert!(read_all_groups(&mut r, 1, 5, false).is_err());
    }
}
