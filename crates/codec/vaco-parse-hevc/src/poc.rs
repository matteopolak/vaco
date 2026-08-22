//! Picture order count, ITU-T H.265 §8.3.1.
//!
//! # Why a clause-8 procedure is on the near side of the parse/decode line
//!
//! Clause 8 is the decoding process, and this crate implements none of it —
//! except this. §8.3.1 is integer arithmetic over two slice-header fields and
//! two remembered values. It needs no reference picture, touches no sample, and
//! produces an output *order* rather than an output *picture*. Reporting that a
//! stream's pictures arrive out of display order, and by how much, is a
//! property of the headers.
//!
//! `vaco-parse-h264` draws the same line for the same reason (§8.2.1 there), and
//! HEVC's version is markedly simpler: one POC type instead of three, no
//! `frame_num` gaps, no field/frame split.
//!
//! # The rule
//!
//! ```text
//!   if the picture is an IRAP with NoRaslOutputFlag:
//!       PicOrderCntMsb = 0
//!   else if lsb < prevLsb  and  prevLsb - lsb >= MaxLsb / 2:
//!       PicOrderCntMsb = prevMsb + MaxLsb
//!   else if lsb > prevLsb  and  lsb - prevLsb  >  MaxLsb / 2:
//!       PicOrderCntMsb = prevMsb - MaxLsb
//!   else:
//!       PicOrderCntMsb = prevMsb
//!   PicOrderCntVal = PicOrderCntMsb + lsb
//! ```
//!
//! Note the asymmetry — `>=` on one side and `>` on the other. It is in the
//! specification and it decides the wrap direction for a picture exactly half a
//! cycle away.

use crate::nal::NalUnitType;
use crate::slice::SliceHeader;
use crate::sps::Sps;

/// The picture order count of one picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub struct PictureOrderCount {
    /// `PicOrderCntVal`, §8.3.1.
    pub value: i64,
    /// `PicOrderCntMsb`.
    pub msb: i64,
    /// `slice_pic_order_cnt_lsb`.
    pub lsb: u32,
}

/// The two values §8.3.1 carries between pictures.
///
/// "Previous" means the previous picture **in decoding order** that has
/// `TemporalId == 0` and is neither a RASL, a RADL, nor a sub-layer
/// non-reference picture (§8.3.1). Applying that filter is what stops a
/// throwaway leading picture from moving the reference point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PocState {
    prev_lsb: u32,
    prev_msb: i64,
    /// Whether a picture has been seen at all, so the first one does not wrap
    /// against a zero that means "nothing yet".
    started: bool,
}

impl PocState {
    /// A fresh state, as after a seek.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            prev_lsb: 0,
            prev_msb: 0,
            started: false,
        }
    }

    /// Forget everything — what a seek does.
    pub const fn reset(&mut self) {
        *self = Self::new();
    }

    /// Compute the picture order count of the picture `header` begins, and
    /// advance the state.
    ///
    /// `no_rasl_output` is `NoRaslOutputFlag`, §8.1: true for an IDR, for a BLA,
    /// and for the *first* CRA of a bitstream or the first after an end-of-
    /// sequence unit. A caller that cannot tell should pass `true` only for the
    /// cases it is sure of — passing `false` for a mid-stream CRA is correct and
    /// is what [`PocState::advance`] does by default.
    #[must_use]
    pub fn advance_with(
        &mut self,
        sps: &Sps,
        header: &SliceHeader,
        temporal_id: u8,
        no_rasl_output: bool,
    ) -> PictureOrderCount {
        let max_lsb = i64::from(sps.max_pic_order_cnt_lsb());
        let lsb = header.pic_order_cnt_lsb;

        let msb = if header.nal_unit_type.is_irap() && no_rasl_output {
            0
        } else if !self.started {
            // §8.3.1's `prevPicOrderCntMsb` is 0 before any picture, so the
            // first non-IRAP picture of a stream joined mid-flight has no
            // wrap to detect.
            0
        } else {
            let l = i64::from(lsb);
            let p = i64::from(self.prev_lsb);
            // `MaxPicOrderCntLsb` is a power of two, so the halving is
            // exact; §8.3.1 writes it as `MaxPicOrderCntLsb / 2`.
            let half = max_lsb >> 1;
            if l < p && (p - l) >= half {
                self.prev_msb.saturating_add(max_lsb)
            } else if l > p && (l - p) > half {
                self.prev_msb.saturating_sub(max_lsb)
            } else {
                self.prev_msb
            }
        };
        let value = msb.saturating_add(i64::from(lsb));

        // §8.3.1: only a TemporalId-0 picture that is not RASL, RADL or a
        // sub-layer non-reference picture updates the reference point.
        let t = header.nal_unit_type;
        if temporal_id == 0 && !t.is_rasl() && !t.is_radl() && !t.is_sub_layer_non_reference() {
            self.prev_lsb = lsb;
            self.prev_msb = msb;
            self.started = true;
        } else if !self.started && t.is_irap() {
            // An IRAP resets the reference point even where the filter above
            // would not update it, because §8.3.1's `prevTid0Pic` is undefined
            // before the first one and 0/0 is the only defensible answer.
            self.started = true;
        }
        PictureOrderCount { value, msb, lsb }
    }

    /// [`PocState::advance_with`] with `NoRaslOutputFlag` inferred: true for an
    /// IDR or a BLA, and for a CRA only when nothing has been seen yet.
    #[must_use]
    pub fn advance(
        &mut self,
        sps: &Sps,
        header: &SliceHeader,
        temporal_id: u8,
    ) -> PictureOrderCount {
        let t = header.nal_unit_type;
        let no_rasl_output = t.is_idr() || t.is_bla() || (t.is_cra() && !self.started);
        self.advance_with(sps, header, temporal_id, no_rasl_output)
    }

    /// Whether any picture has been seen.
    #[must_use]
    pub const fn started(&self) -> bool {
        self.started
    }
}

/// Whether a NAL unit type ends a coded video sequence, so the next IRAP has
/// `NoRaslOutputFlag` set again (§8.1).
#[must_use]
pub const fn ends_sequence(t: NalUnitType) -> bool {
    t.get() == NalUnitType::EOS_NUT.get() || t.get() == NalUnitType::EOB_NUT.get()
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code over fixed fixtures"
)]
mod tests {
    use super::*;
    use crate::slice::SliceKind;

    fn sps(log2_max_lsb: u8) -> Sps {
        Sps {
            log2_max_pic_order_cnt_lsb: log2_max_lsb,
            ..Sps::default()
        }
    }

    fn header(t: NalUnitType, lsb: u32) -> SliceHeader {
        SliceHeader {
            nal_unit_type: t,
            first_slice_segment_in_pic: true,
            pic_order_cnt_lsb: lsb,
            kind: SliceKind::I,
            ..SliceHeader::default()
        }
    }

    /// An IDR is always zero, whatever came before.
    #[test]
    fn an_idr_resets_the_count() {
        let s = sps(8);
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 200), 0);
        let poc = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        assert_eq!(poc.value, 0);
        assert_eq!(poc.msb, 0);
    }

    /// A stream that counts past `MaxPicOrderCntLsb` wraps once, and keeps
    /// counting upwards.
    #[test]
    fn the_msb_wraps_upwards_at_the_cycle_boundary() {
        // log2 = 4 -> MaxPicOrderCntLsb = 16, half = 8.
        let s = sps(4);
        let mut state = PocState::new();
        let mut seen = Vec::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        for lsb in [4u32, 8, 12, 0, 4, 8, 12, 0, 4] {
            seen.push(
                state
                    .advance(&s, &header(NalUnitType::TRAIL_R, lsb), 0)
                    .value,
            );
        }
        assert_eq!(seen, [4, 8, 12, 16, 20, 24, 28, 32, 36]);
    }

    /// A picture that arrives out of order — a B picture whose count is below
    /// the previous one but by less than half a cycle — does **not** wrap.
    #[test]
    fn reordering_within_half_a_cycle_does_not_wrap() {
        let s = sps(4); // MaxLsb 16, half 8
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        let a = state.advance(&s, &header(NalUnitType::TRAIL_R, 8), 0);
        assert_eq!(a.value, 8);
        // 8 -> 4: a drop of 4, less than half, so no wrap.
        let b = state.advance(&s, &header(NalUnitType::TRAIL_R, 4), 0);
        assert_eq!(b.value, 4);
    }

    /// The asymmetric comparison: exactly half a cycle **down** wraps, exactly
    /// half a cycle **up** does not.
    #[test]
    fn the_half_cycle_boundary_is_asymmetric() {
        let s = sps(4); // MaxLsb 16, half 8
        let mut down = PocState::new();
        let _ = down.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        // Climb to 9 in two steps; 0 -> 9 in one would itself exceed half a
        // cycle and wrap downwards, which is the thing being measured.
        let _ = down.advance(&s, &header(NalUnitType::TRAIL_R, 5), 0);
        let _ = down.advance(&s, &header(NalUnitType::TRAIL_R, 9), 0);
        // 9 -> 1 is a drop of exactly 8: `>= half` is true, so it wraps.
        assert_eq!(
            down.advance(&s, &header(NalUnitType::TRAIL_R, 1), 0).value,
            17
        );

        let mut up = PocState::new();
        let _ = up.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        let _ = up.advance(&s, &header(NalUnitType::TRAIL_R, 1), 0);
        // 1 -> 9 is a rise of exactly 8: `> half` is false, so it does not.
        assert_eq!(up.advance(&s, &header(NalUnitType::TRAIL_R, 9), 0).value, 9);
    }

    /// A RASL picture does not move the reference point, so a wrap detected
    /// after it is measured from the last picture that did.
    #[test]
    fn a_rasl_picture_does_not_move_the_reference_point() {
        let s = sps(4);
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 4), 0);
        // A RASL at lsb 12 computes its own count but leaves prev at 4.
        let rasl = state.advance(&s, &header(NalUnitType::RASL_R, 12), 0);
        assert_eq!(rasl.value, 12);
        // The next trailing picture is still measured against 4.
        let next = state.advance(&s, &header(NalUnitType::TRAIL_R, 8), 0);
        assert_eq!(next.value, 8);
    }

    /// A sub-layer non-reference picture, and a picture above temporal layer 0,
    /// are both filtered out of the reference point.
    #[test]
    fn only_temporal_layer_zero_reference_pictures_update_the_state() {
        let s = sps(4);
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 2), 0);
        // TRAIL_N is a sub-layer non-reference picture.
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_N, 6), 0);
        // TemporalId 1 is above layer 0.
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 7), 1);
        // ...so the next one is still measured against lsb 2.
        assert_eq!(
            state.advance(&s, &header(NalUnitType::TRAIL_R, 4), 0).value,
            4
        );
    }

    /// A count that would overflow saturates rather than wrapping into a
    /// negative order.
    #[test]
    fn an_absurd_cycle_saturates_rather_than_overflowing() {
        let s = sps(16); // MaxLsb 65536
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        for _ in 0..64 {
            // Alternate across the boundary so every step wraps upwards.
            let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 65_000), 0);
            let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 100), 0);
        }
        let poc = state.advance(&s, &header(NalUnitType::TRAIL_R, 200), 0);
        assert!(poc.value > 0, "the count stayed positive");
    }

    #[test]
    fn a_reset_forgets_everything() {
        let s = sps(8);
        let mut state = PocState::new();
        let _ = state.advance(&s, &header(NalUnitType::IDR_W_RADL, 0), 0);
        let _ = state.advance(&s, &header(NalUnitType::TRAIL_R, 200), 0);
        assert!(state.started());
        state.reset();
        assert!(!state.started());
        assert_eq!(
            state.advance(&s, &header(NalUnitType::TRAIL_R, 5), 0).value,
            5
        );
    }

    #[test]
    fn the_sequence_terminators_are_recognised() {
        assert!(ends_sequence(NalUnitType::EOS_NUT));
        assert!(ends_sequence(NalUnitType::EOB_NUT));
        assert!(!ends_sequence(NalUnitType::TRAIL_R));
    }
}
