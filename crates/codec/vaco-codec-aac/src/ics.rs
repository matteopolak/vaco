//! `ics_info()` — ISO/IEC 14496-3 subpart 4 Table 4.6: window sequence and
//! shape, `max_sfb`, and (for `EIGHT_SHORT_SEQUENCE`) the scalefactor-band
//! grouping across the block's eight short windows.
//!
//! # What is deliberately not read past
//!
//! `predictor_data_present` is read (it must be, to know whether more bits
//! follow), but this crate rejects a `1` with `Error::Unsupported` rather
//! than parsing further: for `audioObjectType != 1` (AAC Main) — which
//! includes LC — a `1` here is followed by `ltp_data_present` and possibly
//! `ltp_data()` (Table 4.6/4.55), a syntax this crate has not transcribed
//! and would not know what to do with even if it had (LTP/main prediction
//! is #445's explicit scope, "LTP/main prediction"). Real AAC-LC encoders
//! never set this bit — LC has no prediction tool in the first place — so
//! this is not expected to reject real content; it is a "gate rather than
//! guess" refusal for the syntactically-legal-but-unverified case, the same
//! call this workspace has made repeatedly this session.

use vaco_bitstream::BitReader;
use vaco_core::{Error, Result};

/// `window_sequence`, ISO/IEC 14496-3 subpart 4 Table 4.72.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowSequence {
    OnlyLong,
    LongStart,
    EightShort,
    LongStop,
}

impl WindowSequence {
    const fn from_bits(v: u32) -> Self {
        match v {
            0 => Self::OnlyLong,
            1 => Self::LongStart,
            3 => Self::LongStop,
            _ => Self::EightShort, // v == 2, and `get(2)` cannot produce > 3
        }
    }

    /// How many of the 128-line short windows this sequence's block holds:
    /// 8 for `EightShort`, 1 (a single 1024-line window) for everything else.
    pub(crate) const fn num_windows(self) -> usize {
        if matches!(self, Self::EightShort) {
            8
        } else {
            1
        }
    }

    /// Whether this is the short-window sequence — determines which of
    /// every window-size-dependent field width (`max_sfb`, TNS's `n_filt`
    /// etc.) applies.
    pub(crate) const fn is_short(self) -> bool {
        matches!(self, Self::EightShort)
    }
}

/// A parsed `ics_info()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IcsInfo {
    pub(crate) window_sequence: WindowSequence,
    /// `window_shape`: `false` = sine (`vaco-codec-dsp-sinewin`'s scope),
    /// `true` = KBD (unimplemented anywhere in this workspace yet — #445
    /// must gate on this before applying a window, since this crate's own
    /// syntax layer has no reason to reject it).
    pub(crate) window_shape: bool,
    /// Scalefactor bands transmitted per group (long: per frame; short: per
    /// window group).
    pub(crate) max_sfb: u8,
    /// One entry per of the 8 raw short windows (only meaningful for
    /// `EightShort`): `true` means "starts a new group", `false` means
    /// "continues the previous window's group". Always `[true, false, false,
    /// false, false, false, false, false]` (one group) for a long sequence.
    pub(crate) group_starts: [bool; 8],
}

impl IcsInfo {
    /// Number of window groups `scale_factor_grouping` implies: one per
    /// `true` in [`Self::group_starts`], bounded to
    /// [`WindowSequence::num_windows`].
    pub(crate) fn num_window_groups(&self) -> usize {
        self.group_starts
            .iter()
            .take(self.window_sequence.num_windows())
            .filter(|&&s| s)
            .count()
    }

    /// How many of the raw windows belong to each group, in group order.
    /// Length is [`Self::num_window_groups`].
    pub(crate) fn window_group_lengths(&self) -> Vec<u8> {
        let mut lengths = Vec::new();
        for &starts in self
            .group_starts
            .iter()
            .take(self.window_sequence.num_windows())
        {
            if starts || lengths.is_empty() {
                lengths.push(1);
            } else if let Some(last) = lengths.last_mut() {
                *last += 1;
            }
        }
        lengths
    }

    /// Read `ics_info()`. `audio_object_type` selects the
    /// `predictor_data_present` branch (see module doc); this crate always
    /// passes the AAC-LC value, so the `audioObjectType == 1` (Main) branch
    /// is never taken.
    ///
    /// # Errors
    ///
    /// [`Error::UnexpectedEof`] on truncation, [`Error::Unsupported`] if
    /// `predictor_data_present` is set (see module doc).
    pub(crate) fn read(r: &mut BitReader<'_>) -> Result<Self> {
        let _ics_reserved_bit = r.get_bit();
        let window_sequence = WindowSequence::from_bits(r.get(2));
        let window_shape = r.get_bit() != 0;

        let (max_sfb, group_starts) = if window_sequence.is_short() {
            let max_sfb = r.get(4) as u8;
            let grouping = r.get(7);
            // `scale_factor_grouping` has 7 bits, one per boundary between
            // the 8 raw windows (window 0 always starts group 0). Bit i
            // (MSB first, i.e. bit 6 down to bit 0 for boundary 1..=7) is 1
            // when that boundary's window continues the previous group.
            let mut group_starts = [false; 8];
            group_starts[0] = true;
            for boundary in 1..8u32 {
                let continues = (grouping >> (7 - boundary)) & 1 != 0;
                if let Some(slot) = group_starts.get_mut(boundary as usize) {
                    *slot = !continues;
                }
            }
            (max_sfb, group_starts)
        } else {
            let max_sfb = r.get(6) as u8;
            let predictor_data_present = r.get_bit() != 0;
            if predictor_data_present {
                return Err(Error::Unsupported(
                    "vaco-codec-aac: ics_info predictor_data_present is set; LTP/main \
                     prediction is not implemented (#445) and no real AAC-LC encoder \
                     sets this bit, so it is refused rather than guessed at",
                ));
            }
            let mut group_starts = [false; 8];
            group_starts[0] = true;
            (max_sfb, group_starts)
        };

        Ok(Self {
            window_sequence,
            window_shape,
            max_sfb,
            group_starts,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{IcsInfo, WindowSequence};
    use vaco_bitstream::{BitReader, BitWriter};

    fn long_ics_bytes(max_sfb: u8, predictor_present: bool) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.put(1, 0); // reserved
        w.put(2, 0); // ONLY_LONG
        w.put(1, 0); // sine
        w.put(6, u32::from(max_sfb));
        w.put(1, u32::from(predictor_present));
        w.finish()
    }

    #[test]
    fn only_long_reads_max_sfb_and_a_single_group() {
        let bytes = long_ics_bytes(40, false);
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::read(&mut r).unwrap();
        assert_eq!(ics.window_sequence, WindowSequence::OnlyLong);
        assert_eq!(ics.max_sfb, 40);
        assert_eq!(ics.num_window_groups(), 1);
        assert_eq!(ics.window_group_lengths(), vec![1]);
    }

    #[test]
    fn predictor_data_present_is_rejected() {
        let bytes = long_ics_bytes(40, true);
        let mut r = BitReader::new(&bytes);
        assert!(IcsInfo::read(&mut r).is_err());
    }

    #[test]
    fn eight_short_all_separate_groups() {
        let mut w = BitWriter::new();
        w.put(1, 0);
        w.put(2, 2); // EIGHT_SHORT
        w.put(1, 0);
        w.put(4, 12); // max_sfb
        w.put(7, 0b000_0000); // every boundary starts a new group
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::read(&mut r).unwrap();
        assert_eq!(ics.window_sequence, WindowSequence::EightShort);
        assert_eq!(ics.num_window_groups(), 8);
        assert_eq!(ics.window_group_lengths(), vec![1, 1, 1, 1, 1, 1, 1, 1]);
    }

    #[test]
    fn eight_short_all_one_group() {
        let mut w = BitWriter::new();
        w.put(1, 0);
        w.put(2, 2);
        w.put(1, 0);
        w.put(4, 12);
        w.put(7, 0b111_1111); // every boundary continues the group
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::read(&mut r).unwrap();
        assert_eq!(ics.num_window_groups(), 1);
        assert_eq!(ics.window_group_lengths(), vec![8]);
    }

    #[test]
    fn eight_short_mixed_grouping() {
        // boundaries (1..=7): continue,new,continue,continue,new,new,continue
        // groups: [0,1] [2,3,4] [5] [6,7]
        let mut w = BitWriter::new();
        w.put(1, 0);
        w.put(2, 2);
        w.put(1, 0);
        w.put(4, 12);
        w.put(7, 0b101_1001);
        let bytes = w.finish();
        let mut r = BitReader::new(&bytes);
        let ics = IcsInfo::read(&mut r).unwrap();
        assert_eq!(ics.num_window_groups(), 4);
        assert_eq!(ics.window_group_lengths(), vec![2, 3, 1, 2]);
    }
}
