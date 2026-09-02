//! The compressed frame header, RFC 6386 §9 (Annex A §19.2 for the
//! authoritative bit order) and §10 (segment-based feature adjustment).
//!
//! # `segment_feature_mode`'s polarity — the RFC contradicts itself
//!
//! §9.3's overview prose says "absolute-value mode (0) or delta value mode
//! (1)"; Annex A's own field table for the same bit says the opposite ("0
//! for delta and 1 for the absolute value"), and the reference decoder's own
//! source matches Annex A. This crate implements the Annex A polarity
//! (`0 = delta, 1 = absolute`) since that is what every real-world encoder
//! (`libvpx`) produces — measured indirectly, since a decoder built on the
//! §9.3 prose's polarity would silently invert every segment's quantizer and
//! loop-filter level on any stream that uses segmentation.

use vaco_codec_msac::Vp8BoolDecoder as Bd;

use crate::tables;

/// One segment's quantizer/loop-filter override and the map's own state.
#[derive(Debug, Clone)]
pub struct Segmentation {
    pub enabled: bool,
    /// Whether *this frame* codes a fresh per-macroblock segment id (if
    /// false, macroblocks keep whatever segment id a previous frame's map
    /// assigned them — RFC 6386 §9.3/§10).
    pub update_map: bool,
    /// `true`: the four `quant_idx`/`lf_level` entries are absolute values.
    /// `false`: they are deltas against the frame-level baseline. See the
    /// module doc for why this polarity, not the RFC's own contradicted
    /// prose reading.
    pub absolute: bool,
    pub quant_idx: [i32; 4],
    pub lf_level: [i32; 4],
    pub tree_probs: [u8; 3],
}

impl Default for Segmentation {
    fn default() -> Self {
        Self {
            enabled: false,
            update_map: false,
            absolute: false,
            quant_idx: [0; 4],
            lf_level: [0; 4],
            tree_probs: [255; 3],
        }
    }
}

/// `mb_lf_adjustments()`, RFC 6386 §9.4 — persistent across frames except
/// where updated.
#[derive(Debug, Clone, Default)]
pub struct LoopFilterDeltas {
    pub enabled: bool,
    pub ref_frame: [i32; 4],
    pub mode: [i32; 4],
}

/// `quant_indices()`, RFC 6386 §9.6.
#[derive(Debug, Clone, Copy, Default)]
pub struct QuantIndices {
    pub y_ac_qi: i32,
    pub y_dc_delta: i32,
    pub y2_dc_delta: i32,
    pub y2_ac_delta: i32,
    pub uv_dc_delta: i32,
    pub uv_ac_delta: i32,
}

/// Persistent entropy state: coefficient, mode and motion-vector
/// probabilities, cumulative across interframes and reset to spec defaults
/// on every key frame. Snapshotted and restored around a frame whose
/// `refresh_entropy_probs` is false (§9.7-9.9).
#[derive(Debug, Clone)]
pub struct EntropyContext {
    pub coeff_probs: [[[[u8; 11]; 3]; 8]; 4],
    pub mv_probs: [[u8; 19]; 2],
    pub ymode_prob: [u8; 4],
    pub uv_mode_prob: [u8; 3],
}

impl Default for EntropyContext {
    fn default() -> Self {
        Self {
            coeff_probs: tables::DEFAULT_COEFF_PROBS,
            mv_probs: tables::DEFAULT_MV_CONTEXT,
            ymode_prob: tables::YMODE_PROB_DEFAULT,
            uv_mode_prob: tables::UV_MODE_PROB_DEFAULT,
        }
    }
}

/// The fully parsed compressed frame header (everything through the
/// mode-probability and MV-probability updates, i.e. all of RFC 6386 §9
/// except the macroblock-by-macroblock records that follow it).
#[allow(
    clippy::struct_excessive_bools,
    reason = "RFC 6386 §9's frame header genuinely has this many independent flags"
)]
#[derive(Debug, Clone)]
pub struct FrameHeader {
    pub key_frame: bool,
    pub version: u8,
    pub show_frame: bool,
    pub width: u16,
    pub height: u16,
    pub color_space: u32,
    pub clamping_type: u32,
    pub segmentation: Segmentation,
    pub filter_simple: bool,
    pub filter_level: i32,
    pub sharpness_level: i32,
    pub lf_deltas: LoopFilterDeltas,
    pub num_partitions: usize,
    pub quant: QuantIndices,
    pub refresh_golden: bool,
    pub refresh_altref: bool,
    pub copy_to_golden: u32,
    pub copy_to_altref: u32,
    pub sign_bias_golden: bool,
    pub sign_bias_altref: bool,
    pub refresh_entropy_probs: bool,
    pub refresh_last: bool,
    pub mb_no_skip_coeff: bool,
    pub prob_skip_false: u8,
    pub prob_intra: u8,
    pub prob_last: u8,
    pub prob_gf: u8,
}

fn read_delta(bd: &mut Bd<'_>, magnitude_bits: u32) -> i32 {
    if bd.read_flag() {
        bd.read_magnitude_and_sign(magnitude_bits)
    } else {
        0
    }
}

fn update_segmentation(bd: &mut Bd<'_>, seg: &mut Segmentation) {
    seg.update_map = bd.read_flag();
    let update_map = seg.update_map;
    let update_data = bd.read_flag();
    if update_data {
        seg.absolute = bd.read_flag();
        for q in &mut seg.quant_idx {
            *q = read_delta(bd, 7);
        }
        for l in &mut seg.lf_level {
            *l = read_delta(bd, 6);
        }
    }
    if update_map {
        for p in &mut seg.tree_probs {
            *p = if bd.read_flag() {
                bd.read_literal(8) as u8
            } else {
                255
            };
        }
    }
}

fn mb_lf_adjustments(bd: &mut Bd<'_>, lf: &mut LoopFilterDeltas) {
    lf.enabled = bd.read_flag();
    if lf.enabled && bd.read_flag() {
        for d in &mut lf.ref_frame {
            if bd.read_flag() {
                *d = bd.read_magnitude_and_sign(6);
            }
        }
        for d in &mut lf.mode {
            if bd.read_flag() {
                *d = bd.read_magnitude_and_sign(6);
            }
        }
    }
}

fn quant_indices(bd: &mut Bd<'_>) -> QuantIndices {
    QuantIndices {
        y_ac_qi: bd.read_literal(7).cast_signed(),
        y_dc_delta: read_delta(bd, 4),
        y2_dc_delta: read_delta(bd, 4),
        y2_ac_delta: read_delta(bd, 4),
        uv_dc_delta: read_delta(bd, 4),
        uv_ac_delta: read_delta(bd, 4),
    }
}

fn update_coeff_probs(bd: &mut Bd<'_>, probs: &mut [[[[u8; 11]; 3]; 8]; 4]) {
    for (i, plane) in probs.iter_mut().enumerate() {
        for (j, band) in plane.iter_mut().enumerate() {
            for (k, ctx) in band.iter_mut().enumerate() {
                for (t, p) in ctx.iter_mut().enumerate() {
                    let update_prob = tables::COEFF_UPDATE_PROBS
                        .get(i)
                        .and_then(|p| p.get(j))
                        .and_then(|p| p.get(k))
                        .and_then(|p| p.get(t))
                        .copied()
                        .unwrap_or(255);
                    if bd.read_bool(update_prob) {
                        *p = bd.read_literal(8) as u8;
                    }
                }
            }
        }
    }
}

fn update_mv_probs(bd: &mut Bd<'_>, mvc: &mut [[u8; 19]; 2]) {
    for (i, comp) in mvc.iter_mut().enumerate() {
        let update = tables::MV_UPDATE_PROBS.get(i);
        for (j, p) in comp.iter_mut().enumerate() {
            let up = update.and_then(|u| u.get(j)).copied().unwrap_or(255);
            if bd.read_bool(up) {
                let x = bd.read_literal(7);
                *p = if x != 0 { (x << 1) as u8 } else { 1 };
            }
        }
    }
}

/// Parse the compressed frame header, applying coefficient/MV/mode
/// probability updates directly onto `entropy` (the caller is responsible
/// for snapshotting it first if `refresh_entropy_probs` will end up false —
/// see [`EntropyContext`]'s doc).
///
/// `tag` is the already-parsed 3-byte frame tag / key-frame dimensions
/// (`vaco_parse_vpx::vp8::FrameTag`); this function starts reading at the
/// first bit of the boolean-coded first partition.
#[allow(
    clippy::too_many_lines,
    reason = "one linear bitstream walk, RFC 6386 Annex A §19.2's own shape"
)]
#[allow(clippy::too_many_arguments, reason = "one linear bitstream walk")]
pub fn parse(
    bd: &mut Bd<'_>,
    key_frame: bool,
    version: u8,
    show_frame: bool,
    size: Option<(u16, u16)>,
    entropy: &mut EntropyContext,
    segmentation: &mut Segmentation,
    lf_deltas: &mut LoopFilterDeltas,
) -> FrameHeader {
    let (width, height) = size.unwrap_or((0, 0));

    let (color_space, clamping_type) = if key_frame {
        (bd.read_literal(1), bd.read_literal(1))
    } else {
        (0, 0)
    };

    segmentation.enabled = bd.read_flag();
    segmentation.update_map = false;
    if segmentation.enabled {
        update_segmentation(bd, segmentation);
    }

    let filter_simple = bd.read_flag();
    let filter_level = bd.read_literal(6).cast_signed();
    let sharpness_level = bd.read_literal(3).cast_signed();

    mb_lf_adjustments(bd, lf_deltas);

    let num_partitions = 1usize << bd.read_literal(2);
    let quant = quant_indices(bd);

    let (
        refresh_golden,
        refresh_altref,
        copy_to_golden,
        copy_to_altref,
        sign_bias_golden,
        sign_bias_altref,
        refresh_entropy_probs,
        refresh_last,
    ) = if key_frame {
        let refresh_entropy_probs = bd.read_flag();
        (true, true, 0, 0, false, false, refresh_entropy_probs, true)
    } else {
        let refresh_golden = bd.read_flag();
        let refresh_altref = bd.read_flag();
        let copy_to_golden = if refresh_golden {
            0
        } else {
            bd.read_literal(2)
        };
        let copy_to_altref = if refresh_altref {
            0
        } else {
            bd.read_literal(2)
        };
        let sign_bias_golden = bd.read_flag();
        let sign_bias_altref = bd.read_flag();
        let refresh_entropy_probs = bd.read_flag();
        let refresh_last = bd.read_flag();
        (
            refresh_golden,
            refresh_altref,
            copy_to_golden,
            copy_to_altref,
            sign_bias_golden,
            sign_bias_altref,
            refresh_entropy_probs,
            refresh_last,
        )
    };

    update_coeff_probs(bd, &mut entropy.coeff_probs);

    let mb_no_skip_coeff = bd.read_flag();
    let prob_skip_false = if mb_no_skip_coeff {
        bd.read_literal(8) as u8
    } else {
        0
    };

    let mut prob_intra = 0u8;
    let mut prob_last = 0u8;
    let mut prob_gf = 0u8;
    if !key_frame {
        prob_intra = bd.read_literal(8) as u8;
        prob_last = bd.read_literal(8) as u8;
        prob_gf = bd.read_literal(8) as u8;
        if bd.read_flag() {
            for p in &mut entropy.ymode_prob {
                *p = bd.read_literal(8) as u8;
            }
        }
        if bd.read_flag() {
            for p in &mut entropy.uv_mode_prob {
                *p = bd.read_literal(8) as u8;
            }
        }
        update_mv_probs(bd, &mut entropy.mv_probs);
    }

    FrameHeader {
        key_frame,
        version,
        show_frame,
        width,
        height,
        color_space,
        clamping_type,
        segmentation: segmentation.clone(),
        filter_simple,
        filter_level,
        sharpness_level,
        lf_deltas: lf_deltas.clone(),
        num_partitions,
        quant,
        refresh_golden,
        refresh_altref,
        copy_to_golden,
        copy_to_altref,
        sign_bias_golden,
        sign_bias_altref,
        refresh_entropy_probs,
        refresh_last,
        mb_no_skip_coeff,
        prob_skip_false,
        prob_intra,
        prob_last,
        prob_gf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal key-frame header: no segmentation, normal filter, level 0,
    /// sharpness 0, no lf deltas, 1 partition, y_ac_qi=42 with no deltas,
    /// refresh_entropy_probs=1, no coeff prob updates, mb_no_skip_coeff=0.
    #[allow(
        clippy::vec_init_then_push,
        reason = "each push is commented with the syntax element it encodes; a vec![] literal would lose that"
    )]
    fn minimal_key_frame_bits() -> Vec<bool> {
        let mut bits = Vec::new();
        bits.push(false); // color_space
        bits.push(false); // clamping_type
        bits.push(false); // segmentation_enabled
        bits.push(false); // filter_type (normal)
        bits.extend([false; 6]); // filter_level = 0
        bits.extend([false; 3]); // sharpness_level = 0
        bits.push(false); // loop_filter_adj_enable
        bits.extend([false, false]); // log2_nbr_of_dct_partitions = 0 -> 1 partition
        bits.extend([false; 7]); // y_ac_qi = 0
        bits.extend([false; 5]); // 5 delta-present flags, all 0
        bits.push(true); // refresh_entropy_probs = 1
        // 1056 coeff update flags, all coded at fixed nonzero probs; supply all-zero (no update)
        bits.extend(std::iter::repeat_n(false, 1056));
        bits.push(false); // mb_no_skip_coeff = 0
        bits
    }

    fn bits_to_bool_encoded(bits: &[bool]) -> Vec<u8> {
        // Encode each bit as a plain literal bool at prob 128 using the
        // same encoder vaco-codec-msac's vp8 tests use, so the resulting
        // partition decodes back through the real BoolDecoder.
        struct Enc {
            out: Vec<u8>,
            range: u32,
            bottom: u32,
            bit_count: i32,
        }
        impl Enc {
            fn new() -> Self {
                Self {
                    out: Vec::new(),
                    range: 255,
                    bottom: 0,
                    bit_count: 24,
                }
            }
            fn carry(&mut self) {
                for b in self.out.iter_mut().rev() {
                    if *b == 255 {
                        *b = 0;
                    } else {
                        *b += 1;
                        return;
                    }
                }
            }
            fn write(&mut self, prob: u8, v: bool) {
                let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
                if v {
                    self.bottom += split;
                    self.range -= split;
                } else {
                    self.range = split;
                }
                while self.range < 128 {
                    self.range <<= 1;
                    if self.bottom & (1 << 31) != 0 {
                        self.carry();
                    }
                    self.bottom <<= 1;
                    self.bit_count -= 1;
                    if self.bit_count == 0 {
                        self.out.push((self.bottom >> 24) as u8);
                        self.bottom &= (1 << 24) - 1;
                        self.bit_count = 8;
                    }
                }
            }
            fn finish(mut self) -> Vec<u8> {
                let mut c = self.bit_count;
                let mut v = self.bottom;
                if v & (1 << (32 - c)) != 0 {
                    self.carry();
                }
                v <<= c & 7;
                c >>= 3;
                for _ in 0..c {
                    v <<= 8;
                }
                for _ in 0..4 {
                    self.out.push((v >> 24) as u8);
                    v <<= 8;
                }
                self.out
            }
        }
        let mut e = Enc::new();
        for &b in bits {
            e.write(128, b);
        }
        e.finish()
    }

    #[test]
    fn a_minimal_key_frame_header_parses() {
        let bytes = bits_to_bool_encoded(&minimal_key_frame_bits());
        let mut bd = Bd::new(&bytes);
        let mut entropy = EntropyContext::default();
        let mut segmentation = Segmentation::default();
        let mut lf_deltas = LoopFilterDeltas::default();
        let hdr = parse(
            &mut bd,
            true,
            0,
            true,
            Some((16, 16)),
            &mut entropy,
            &mut segmentation,
            &mut lf_deltas,
        );
        assert!(hdr.key_frame);
        assert_eq!(hdr.num_partitions, 1);
        assert_eq!(hdr.quant.y_ac_qi, 0);
        assert!(hdr.refresh_entropy_probs);
        assert!(!hdr.mb_no_skip_coeff);
        assert!(!bd.overrun());
    }
}
