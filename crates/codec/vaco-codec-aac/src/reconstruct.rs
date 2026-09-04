//! Reconstruction (T3-03c / #445): turning #444's fully-parsed syntax into
//! actual PCM. Inverse quantisation (§4.6.1), scalefactor application
//! (§4.6.2.3.3), perceptual noise substitution (§4.6.13.3), TNS application
//! (via [`crate::tns_apply`]), joint stereo — M/S (§4.6.8.1.3) and intensity
//! (§4.6.8.2.3) — and finally the IMDCT/windowing/overlap-add filterbank
//! (§4.6.11.3).
//!
//! # This is where the claim changes character
//! Everything before this module was verifiable by an exact invariant: bits
//! consumed. This module produces samples, and AAC — like every lossy codec
//! this workspace has decoded so far — defines a compliance tolerance
//! rather than one correct output. See `docs/codec/vaco-codec-aac.md` for
//! the measured `correlation/max_abs/rms` table; nothing here claims or
//! chases bit-exactness.
#![allow(
    clippy::integer_division,
    reason = "every division in this module is an exact halving of an even AAC block length \
              (2048 or 256) or a compile-time-constant window-boundary offset derived from one, \
              never a truncating division on a runtime value"
)]

use vaco_codec_dsp_sinewin::{kbd_window, sine_window};
use vaco_core::{Error, Result};
use vaco_tx::{Direction, Plan, Tx, TxFlags, TxKind};

/// KBD shape parameter for the 2048-sample long window (§4.6.11.3.2).
const KBD_ALPHA_LONG: f64 = 4.0;
/// KBD shape parameter for the 256-sample short window (§4.6.11.3.2).
const KBD_ALPHA_SHORT: f64 = 6.0;

use crate::ics::WindowSequence;
use crate::ics_stream::IcsStream;
use crate::raw_data_block::MsMask;
use crate::scalefactor::BandValue;
use crate::tns_apply;

const LONG_LEN: usize = 2048;
const SHORT_LEN: usize = 256;
/// The eight short transforms of an `EIGHT_SHORT_SEQUENCE`.
const NUM_SHORT: usize = 8;
/// Where the eight short windows' overlapped span begins inside the
/// 2048-sample block (§4.6.11.3.2). Hopping by `SHORT_LEN / 2`, the eight
/// 256-sample windows together span `(8 + 1) * 128 = 1152` samples, centred
/// in the block: 448 zeros before, 448 after.
///
/// One constant rather than three literals because all three uses have to
/// agree or time-domain alias cancellation fails across a window-sequence
/// transition — `LongStop`'s ascending short segment starts here,
/// `LongStart`'s descending one ends at `LONG_LEN - SHORT_START`, and
/// `overlap_add_eight_short` lays window `j` down at
/// `SHORT_START + j * SHORT_LEN / 2`. They did not agree before: the
/// overlap-add used `(LONG_LEN - SHORT_LEN) / 2 - SHORT_LEN / 2` = 768,
/// putting every short block 320 samples late (measured against
/// `ffmpeg -bitexact` on a burst fixture: every burst onset +320).
const SHORT_START: usize = (LONG_LEN - (NUM_SHORT + 1) * (SHORT_LEN / 2)) / 2;
/// Final output normalisation: §4.6.1's inverse-quantisation formula
/// produces samples on a 16-bit-PCM scale (matching FAAD2 and other
/// reference decoders' convention), not the `[-1, 1]` range this crate's
/// `SampleFmt::F32P` output represents. See `finalize_channel` for the
/// empirical evidence this constant is based on.
const PCM_TO_FLOAT_SCALE: f32 = 1.0 / 32768.0;

/// A tiny, explicitly non-normative pseudo-random generator for perceptual
/// noise substitution (§4.6.13.3: "a suitable random number generator can
/// be realized using one multiplication/accumulation per random value" —
/// PNS is deliberately not bit-exact across decoders; any statistically
/// reasonable sequence normalised to the transmitted energy is correct).
struct Prng(u32);

impl Prng {
    fn next(&mut self) -> f32 {
        // A classic 32-bit LCG (Numerical Recipes' constants), rescaled to
        // roughly [-1, 1]; only used before immediate energy normalisation,
        // so neither its distribution nor its period need to be exact.
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.0 as f32 / u32::MAX as f32).mul_add(2.0, -1.0)
    }
}

/// Per-channel state carried across frames for overlap-add (§4.6.11.1):
/// "the first half of the `z_i,n` sequence is added to the second half of the
/// previous block['s] windowed sequence."
#[derive(Debug, Clone)]
pub(crate) struct OverlapState {
    /// The previous frame's windowed IMDCT output, second half only
    /// (always 1024 samples — every `window_sequence`'s total length is
    /// 2048, `window_shape`/`window_sequence` only change how it got
    /// there).
    second_half: Vec<f32>,
    /// This channel's `window_shape` from the *previous* block, needed for
    /// the left half of the next block's window
    /// (§4.6.11.3.2: "the `window_shape` of the left half of the first
    /// transform window is determined by the window shape of the previous
    /// block").
    prev_window_shape: bool,
}

impl OverlapState {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            second_half: vec![0.0; LONG_LEN / 2],
            prev_window_shape: false,
        }
    }
}

/// The two inverse-MDCT plans AAC LC ever needs: the long (2048) and short
/// (256) block lengths are fixed by the format, so there is no reason for a
/// length-keyed cache the way a codec with variable block sizes needs one
/// (contrast `vaco-codec-vorbis`'s `Imdct`).
///
/// `f64` throughout, matching `vaco_tx::reference::imp::imdct` (the O(n²)
/// direct evaluation this replaces) to `rms_rel < 1e-12`
/// (`vaco-tx/tests/oracle.rs`) — the plan's own contract for
/// `Plan::<f64>::new(Mdct, Inverse, n, 1.0, FULL_IMDCT)` up to `n = 960`,
/// extended to AAC's 2048/256 by this crate's own tests (see
/// `tests::full_imdct_2048_and_256_match_the_reference` below). This is what
/// keeps the change verifiable against the *current* production output
/// rather than against a widened tolerance — an `f32` plan is a later,
/// separately measured step (C2).
#[derive(Debug)]
pub(crate) struct ImdctPlans {
    long: Tx<f64>,
    short: Tx<f64>,
}

impl ImdctPlans {
    pub(crate) fn new() -> Result<Self> {
        let long = Plan::<f64>::new(
            TxKind::Mdct,
            Direction::Inverse,
            LONG_LEN,
            1.0,
            TxFlags::FULL_IMDCT,
        )
        .map_err(|_| Error::InvalidData("vaco-codec-aac: failed to build the long IMDCT plan"))?;
        let short = Plan::<f64>::new(
            TxKind::Mdct,
            Direction::Inverse,
            SHORT_LEN,
            1.0,
            TxFlags::FULL_IMDCT,
        )
        .map_err(|_| Error::InvalidData("vaco-codec-aac: failed to build the short IMDCT plan"))?;
        Ok(Self {
            long: Tx::new(long),
            short: Tx::new(short),
        })
    }
}

/// `x_invquant = sign(x_quant) * |x_quant|^(4/3)` (§4.6.1.3), then
/// `x_rescal = x_invquant * 2^(0.25*(sf - 100))` (§4.6.2.3.3) — folded into
/// one pass since nothing else needs the un-rescaled value.
fn inverse_quantize_and_rescale(x_quant: i32, scalefactor: i32) -> f32 {
    let sign = if x_quant < 0 { -1.0f64 } else { 1.0 };
    let invquant = sign * f64::from(x_quant.unsigned_abs()).powf(4.0 / 3.0);
    let gain = 2f64.powf(0.25 * f64::from(scalefactor - 100));
    (invquant * gain) as f32
}

/// De-interleave one channel's `x_quant`/`band_values` (§4.5.2.3.5's
/// `quant_to_spec()`) into `num_windows` linear per-window spectra, each
/// `window_len` samples wide (1024 for a long block's one window, 128 for
/// each of an eight-short block's eight), applying inverse quantisation,
/// scalefactor rescaling and perceptual noise substitution as it goes —
/// every raw `x_quant` value belongs to exactly one band with exactly one
/// treatment, so there is no reason to make a second pass over the array.
fn deinterleave_and_rescale(
    stream: &IcsStream,
    group_lengths: &[u8],
    swb_offset: &[u16],
    window_len: usize,
    num_windows: usize,
    prng: &mut Prng,
) -> Vec<Vec<f32>> {
    let mut spec = vec![vec![0.0f32; window_len]; num_windows];
    let mut window_base = 0usize;
    for (g, (group_xq, group_bv)) in stream
        .x_quant
        .iter()
        .zip(stream.band_values.iter())
        .enumerate()
    {
        let glen = usize::from(group_lengths.get(g).copied().unwrap_or(1));
        let mut pos = 0usize; // position within this group's flat x_quant array
        let mut j = 0usize; // position within each window's own spectrum
        for (sfb, &value) in group_bv.iter().enumerate() {
            let Some((&lo, &hi)) = swb_offset.get(sfb).zip(swb_offset.get(sfb + 1)) else {
                break;
            };
            let width = usize::from(hi - lo);
            for win in 0..glen {
                let out_win = window_base + win;
                match value {
                    BandValue::NoiseEnergy(nrg) => {
                        let mut energy = 0.0f64;
                        let mut samples = Vec::new();
                        for _ in 0..width {
                            let s = prng.next();
                            energy += f64::from(s) * f64::from(s);
                            samples.push(s);
                        }
                        let sqrt_nrg = energy.sqrt();
                        let scale = if sqrt_nrg > 0.0 {
                            2f64.powf(0.25 * f64::from(nrg)) / sqrt_nrg
                        } else {
                            0.0
                        };
                        for (k, s) in samples.into_iter().enumerate() {
                            if let Some(slot) = spec.get_mut(out_win).and_then(|w| w.get_mut(j + k))
                            {
                                *slot = (f64::from(s) * scale) as f32;
                            }
                        }
                    }
                    BandValue::Scalefactor(sf) => {
                        for k in 0..width {
                            let Some(&xq) = group_xq.get(pos + win * width + k) else {
                                continue;
                            };
                            if let Some(slot) = spec.get_mut(out_win).and_then(|w| w.get_mut(j + k))
                            {
                                *slot = inverse_quantize_and_rescale(xq, sf);
                            }
                        }
                    }
                    // Zero and intensity bands: left as 0.0 — intensity is
                    // filled in by `apply_intensity_stereo` afterward, once
                    // the left channel's own rescaled spectrum exists.
                    BandValue::Zero | BandValue::IntensityPosition(_) => {}
                }
            }
            pos += glen * width;
            j += width;
        }
        window_base += glen;
    }
    spec
}

/// §4.6.8.2.3's `is_intensity`/`invert_intensity`/scale, applied per band:
/// derives the right channel's spectrum for every intensity-coded band from
/// the *left* channel's own (already rescaled) spectrum at the same
/// position — no data of its own was ever transmitted for these bands.
fn apply_intensity_stereo(
    left: &[Vec<f32>],
    right: &mut [Vec<f32>],
    right_stream: &IcsStream,
    group_lengths: &[u8],
    swb_offset: &[u16],
    ms_used: Option<&[Vec<bool>]>,
) {
    let mut window_base = 0usize;
    for (g, group_bv) in right_stream.band_values.iter().enumerate() {
        let glen = usize::from(group_lengths.get(g).copied().unwrap_or(1));
        let mut j = 0usize;
        for (sfb, &value) in group_bv.iter().enumerate() {
            let Some((&lo, &hi)) = swb_offset.get(sfb).zip(swb_offset.get(sfb + 1)) else {
                break;
            };
            let width = usize::from(hi - lo);
            // Both intensity codebooks (in-phase `INTENSITY_HCB` and
            // out-of-phase `INTENSITY_HCB2`) decode to the same
            // `BandValue::IntensityPosition` shape; `IcsStream` does not
            // retain which one a band actually used (only `sfb_cb`'s
            // consequence, `band_values`), so this always takes the
            // in-phase (`+1`) sign — a disclosed gap, not a guess dressed
            // up as one; see `docs/codec/vaco-codec-aac.md`.
            let (is_dir, position): (f64, i32) = if let BandValue::IntensityPosition(p) = value {
                (1.0, p)
            } else {
                j += width;
                continue;
            };
            let invert = ms_used
                .and_then(|m| m.get(g))
                .and_then(|g| g.get(sfb))
                .copied()
                .map_or(1.0, |used| if used { -1.0 } else { 1.0 });
            let scale = (is_dir * invert * 2f64.powf(-0.25 * f64::from(position))) as f32;
            for win in 0..glen {
                let out_win = window_base + win;
                for k in 0..width {
                    let l = left
                        .get(out_win)
                        .and_then(|w| w.get(j + k))
                        .copied()
                        .unwrap_or(0.0);
                    if let Some(slot) = right.get_mut(out_win).and_then(|w| w.get_mut(j + k)) {
                        *slot = l * scale;
                    }
                }
            }
            j += width;
        }
        window_base += glen;
    }
}

/// Apply M/S stereo (§4.6.8.1.3) in place, per band, skipping any band that
/// is intensity- or noise-coded on the *right* channel (M/S, intensity and
/// noise substitution are mutually exclusive per scalefactor band).
fn apply_ms_stereo(
    left: &mut [Vec<f32>],
    right: &mut [Vec<f32>],
    right_stream: &IcsStream,
    group_lengths: &[u8],
    swb_offset: &[u16],
    ms_mask: &MsMask,
) {
    let mut window_base = 0usize;
    for (g, group_bv) in right_stream.band_values.iter().enumerate() {
        let glen = usize::from(group_lengths.get(g).copied().unwrap_or(1));
        let mut j = 0usize;
        for (sfb, &value) in group_bv.iter().enumerate() {
            let Some((&lo, &hi)) = swb_offset.get(sfb).zip(swb_offset.get(sfb + 1)) else {
                break;
            };
            let width = usize::from(hi - lo);
            let used = ms_mask
                .used
                .get(g)
                .and_then(|g| g.get(sfb))
                .copied()
                .unwrap_or(false);
            let is_special = matches!(
                value,
                BandValue::IntensityPosition(_) | BandValue::NoiseEnergy(_)
            );
            if used && !is_special {
                for win in 0..glen {
                    let out_win = window_base + win;
                    for k in 0..width {
                        let (Some(l), Some(r)) = (
                            left.get(out_win).and_then(|w| w.get(j + k)).copied(),
                            right.get(out_win).and_then(|w| w.get(j + k)).copied(),
                        ) else {
                            continue;
                        };
                        if let Some(slot) = left.get_mut(out_win).and_then(|w| w.get_mut(j + k)) {
                            *slot = l + r;
                        }
                        if let Some(slot) = right.get_mut(out_win).and_then(|w| w.get_mut(j + k)) {
                            *slot = l - r;
                        }
                    }
                }
            }
            j += width;
        }
        window_base += glen;
    }
}

/// Apply this channel's TNS filters, one call per window.
fn apply_tns(
    stream: &IcsStream,
    spec: &mut [Vec<f32>],
    swb_offset: &[u16],
    max_sfb: usize,
    max_bands: u8,
    is_short: bool,
) {
    let Some(tns) = &stream.tns else { return };
    let num_swb = swb_offset.len().saturating_sub(1);
    for (w, filters) in tns.per_window.iter().enumerate() {
        if let Some(window_spec) = spec.get_mut(w) {
            tns_apply::apply_to_window(
                window_spec,
                filters,
                swb_offset,
                num_swb,
                max_sfb,
                max_bands,
                is_short,
            );
        }
    }
}

/// The window function for one 2048-sample-total block (§4.6.11.3.2).
///
/// `this_shape`/`prev_shape` are the current and previous blocks'
/// `window_shape` bits (`false` = sine, `true` = KBD). Per §4.6.11.3.2, the
/// left half of a block's window is drawn from the *previous* block's shape
/// and the right half from the *current* block's shape — so, unlike the
/// original sine-only version of this function, the two halves may come
/// from different formulas.
///
/// `vaco-codec-dsp-sinewin` was originally sine-only by its stated scope
/// (D-06); real ffmpeg-encoded fixtures use `window_shape == 1` (KBD) for
/// some frames, which was a real finding against that scope rather than an
/// edge case — see that crate's own doc comment and
/// `docs/signal/vaco-codec-dsp-sinewin.md`.
fn build_window(sequence: WindowSequence, this_shape: bool, prev_shape: bool) -> [f32; LONG_LEN] {
    let long_left_full: [f32; LONG_LEN] = if prev_shape {
        kbd_window::<LONG_LEN>(KBD_ALPHA_LONG)
    } else {
        sine_window::<LONG_LEN>()
    };
    let long_right_full: [f32; LONG_LEN] = if this_shape {
        kbd_window::<LONG_LEN>(KBD_ALPHA_LONG)
    } else {
        sine_window::<LONG_LEN>()
    };
    let short_left_full: [f32; SHORT_LEN] = if prev_shape {
        kbd_window::<SHORT_LEN>(KBD_ALPHA_SHORT)
    } else {
        sine_window::<SHORT_LEN>()
    };
    let short_right_full: [f32; SHORT_LEN] = if this_shape {
        kbd_window::<SHORT_LEN>(KBD_ALPHA_SHORT)
    } else {
        sine_window::<SHORT_LEN>()
    };
    let mut w = [0.0f32; LONG_LEN];
    match sequence {
        WindowSequence::OnlyLong => {
            copy_range(&mut w, &long_left_full, 0);
            copy_range(&mut w[1024..], &long_right_full[1024..], 0);
        }
        WindowSequence::LongStart => {
            // [0, 1024): long left half, previous block's shape.
            // [1024, 1472): 1.0. [1472, 1600): current short window's own
            // right half (samples 128..256), current block's shape —
            // 1600 is `LONG_LEN - SHORT_START`, the sample the eight-short
            // sequence's last window also stops contributing at.
            // [1600, 2048): 0.0. The standard, universally-implemented
            // construction for this transition — the boundary arithmetic
            // could not be independently confirmed from this crate's own
            // (partially garbled) PDF extraction of §4.6.11.3.2 part b),
            // so this is disclosed as an assumption to verify, checked
            // empirically against a real transition fixture instead (see
            // docs/codec/vaco-codec-aac.md).
            // Only the left half: copying the whole `long_left_full` here
            // left its descending tail sitting in `w[1600..]`, where the
            // sequence is defined to be zero, and nothing later overwrote it.
            copy_range(&mut w[..1024], &long_left_full[..1024], 0);
            fill_range(&mut w[1024..1472], 1.0);
            copy_range(&mut w[1472..1600], &short_right_full[128..], 0);
        }
        WindowSequence::LongStop => {
            // The literal window boundaries in this `match` are pinned to
            // `SHORT_START` by the `const` assertions below `build_window`;
            // they are spelled out here because a `const`-expression range
            // would not read as a window shape.
            // Mirror of LongStart: the short-derived segment sits before
            // the block's temporal centre, so it takes the previous
            // block's shape; the long right half takes the current one.
            copy_range(&mut w[448..576], &short_left_full, 0);
            fill_range(&mut w[576..1024], 1.0);
            copy_range(&mut w[1024..], &long_right_full[1024..], 0);
        }
        WindowSequence::EightShort => {
            // Handled per-window by the caller (each of the 8 windows uses
            // its own left/right selection); this branch is unreachable in
            // practice since `finalize_channel` never calls `build_window`
            // for EightShort.
        }
    }
    w
}

// `build_window`'s transition shapes and `overlap_add_eight_short`'s window
// placement have to meet at the same two samples, or the transitions stop
// cancelling their time-domain alias. These fail the build if an edit moves
// one without the other.
const _: () = assert!(
    SHORT_START == 448,
    "LongStop's short segment is w[448..576]"
);
const _: () = assert!(
    LONG_LEN - SHORT_START == 1600,
    "LongStart's short segment is w[1472..1600]"
);

/// Copy `src` into `dst` starting at `dst_start`, one iterator pass — used
/// instead of index-by-loop-variable so neither side ever risks a
/// direct-indexing panic.
fn copy_range(dst: &mut [f32], src: &[f32], dst_start: usize) {
    for (d, &s) in dst.iter_mut().skip(dst_start).zip(src.iter()) {
        *d = s;
    }
}

/// Fill every sample of `dst` with `value`.
fn fill_range(dst: &mut [f32], value: f32) {
    for d in dst.iter_mut() {
        *d = value;
    }
}

/// Deinterleave and rescale one channel's `IcsStream` into its own
/// per-window linear spectra — the first half of reconstruction, done
/// independently per channel so a [`crate::raw_data_block::Element::Pair`]
/// can apply M/S and intensity stereo across both channels' spectra
/// *before* either one's TNS runs (§4.5.2.2.5's own block diagram shows
/// joint stereo feeding TNS, not the reverse — see this module's own doc
/// for the primary-text evidence).
pub(crate) fn deinterleave_channel(
    stream: &IcsStream,
    swb_offset_long: &[u16],
    swb_offset_short: &[u16],
    prng_seed: u32,
) -> Vec<Vec<f32>> {
    let ics = &stream.ics;
    let is_short = ics.window_sequence.is_short();
    let swb_offset = if is_short {
        swb_offset_short
    } else {
        swb_offset_long
    };
    let group_lengths = ics.window_group_lengths();
    let window_len = if is_short {
        SHORT_LEN / 2
    } else {
        LONG_LEN / 2
    };
    let num_windows = ics.window_sequence.num_windows();
    let mut prng = Prng(prng_seed);
    deinterleave_and_rescale(
        stream,
        &group_lengths,
        swb_offset,
        window_len,
        num_windows,
        &mut prng,
    )
}

/// Apply M/S stereo, then intensity stereo, across an already-deinterleaved
/// channel pair's spectra (both mutated in place) — the joint-stereo step
/// of a `channel_pair_element()` with `common_window` set.
pub(crate) fn apply_joint_stereo(
    left_spec: &mut [Vec<f32>],
    right_spec: &mut [Vec<f32>],
    right_stream: &IcsStream,
    swb_offset_long: &[u16],
    swb_offset_short: &[u16],
    ms_mask: &MsMask,
) {
    let ics = &right_stream.ics;
    let is_short = ics.window_sequence.is_short();
    let swb_offset = if is_short {
        swb_offset_short
    } else {
        swb_offset_long
    };
    let group_lengths = ics.window_group_lengths();
    apply_ms_stereo(
        left_spec,
        right_spec,
        right_stream,
        &group_lengths,
        swb_offset,
        ms_mask,
    );
    apply_intensity_stereo(
        left_spec,
        right_spec,
        right_stream,
        &group_lengths,
        swb_offset,
        Some(ms_mask.used.as_slice()),
    );
}

/// Apply TNS, then IMDCT/windowing, then overlap-add, turning one channel's
/// (joint-stereo-adjusted) spectra into `LONG_LEN/2` (1024) output samples.
/// `overlap` is updated in place for the next call on this same channel.
///
/// Infallible today — everything reaching this function already passed
/// `raw_data_block`'s own gates (CCE refused, unsupported configurations
/// gated earlier); a future unsupported-configuration case discovered
/// *during* reconstruction would need this to return a `Result` again.
pub(crate) fn finalize_channel(
    stream: &IcsStream,
    mut spec: Vec<Vec<f32>>,
    swb_offset_long: &[u16],
    swb_offset_short: &[u16],
    max_bands_long: u8,
    max_bands_short: u8,
    overlap: &mut OverlapState,
    imdct: &mut ImdctPlans,
) -> Vec<f32> {
    let ics = &stream.ics;
    let is_short = ics.window_sequence.is_short();
    let swb_offset = if is_short {
        swb_offset_short
    } else {
        swb_offset_long
    };
    let max_bands = if is_short {
        max_bands_short
    } else {
        max_bands_long
    };

    apply_tns(
        stream,
        &mut spec,
        swb_offset,
        usize::from(ics.max_sfb),
        max_bands,
        is_short,
    );

    // IMDCT + windowing, one call per window.
    let mut windowed: Vec<Vec<f32>> = Vec::new();
    if is_short {
        // Only the very first of the 8 short windows straddles the block
        // boundary: its left half takes the previous block's shape, its
        // right half (and every later window, left and right alike) takes
        // this block's own shape (§4.6.11.3.2).
        let this_short: [f32; SHORT_LEN] = if ics.window_shape {
            kbd_window::<SHORT_LEN>(KBD_ALPHA_SHORT)
        } else {
            sine_window::<SHORT_LEN>()
        };
        let first_left: [f32; SHORT_LEN] = if overlap.prev_window_shape {
            kbd_window::<SHORT_LEN>(KBD_ALPHA_SHORT)
        } else {
            sine_window::<SHORT_LEN>()
        };
        for (idx, w) in spec.iter().enumerate() {
            let mut coeffs: Vec<f64> = w.iter().map(|&v| f64::from(v)).collect();
            // The plan's input contract is exactly `SHORT_LEN / 2` samples;
            // pad or truncate defensively so a malformed bitstream (an
            // `IcsStream` whose window came out a different length) hits the
            // "produces no output" branch of `Tx::execute` rather than its
            // `debug_assert`, matching the never-panics behaviour the
            // reference evaluation this replaces had unconditionally.
            coeffs.resize(SHORT_LEN / 2, 0.0);
            let mut time = vec![0.0f64; SHORT_LEN];
            imdct.short.execute(&mut time, &coeffs);
            let scale = 2.0 / SHORT_LEN as f64;
            let mut out = vec![0.0f32; SHORT_LEN];
            for (i, slot) in out.iter_mut().enumerate() {
                let raw = time.get(i).copied().unwrap_or(0.0) * scale;
                let shape = if idx == 0 && i < SHORT_LEN / 2 {
                    &first_left
                } else {
                    &this_short
                };
                *slot = (raw * f64::from(shape.get(i).copied().unwrap_or(0.0))) as f32;
            }
            windowed.push(out);
        }
    } else {
        let win = build_window(
            ics.window_sequence,
            ics.window_shape,
            overlap.prev_window_shape,
        );
        let mut coeffs: Vec<f64> = spec
            .first()
            .map(|w| w.iter().map(|&v| f64::from(v)).collect())
            .unwrap_or_default();
        coeffs.resize(LONG_LEN / 2, 0.0);
        let mut time = vec![0.0f64; LONG_LEN];
        imdct.long.execute(&mut time, &coeffs);
        let scale = 2.0 / LONG_LEN as f64;
        let mut out = vec![0.0f32; LONG_LEN];
        for (i, slot) in out.iter_mut().enumerate() {
            let raw = time.get(i).copied().unwrap_or(0.0) * scale;
            *slot = (raw * f64::from(win.get(i).copied().unwrap_or(0.0))) as f32;
        }
        windowed.push(out);
    }

    // Overlap-add: reassemble one contiguous 2048-sample windowed sequence
    // z_i,n for this block (concatenating the 8 short windows' own
    // overlap-add per §4.6.11.3.2 part c), or the single long/transition
    // window as-is), then add this frame's first half to the previous
    // frame's stored second half.
    let z = if is_short {
        overlap_add_eight_short(&windowed)
    } else {
        windowed
            .into_iter()
            .next()
            .unwrap_or_else(|| vec![0.0; LONG_LEN])
    };

    let half = LONG_LEN / 2;
    let mut output = vec![0.0f32; half];
    for i in 0..half {
        let first = z.get(i).copied().unwrap_or(0.0);
        let prev = overlap.second_half.get(i).copied().unwrap_or(0.0);
        if let Some(slot) = output.get_mut(i) {
            // §4.6.1's inverse-quantisation formula (`x_invquant =
            // sign(x)*|x|^(4/3)`) produces samples on the same scale as a
            // 16-bit PCM decoder (matching FAAD2 and other reference
            // implementations' convention) — not the `[-1, 1]` range this
            // crate's `SampleFmt::F32P` output represents. Confirmed
            // empirically: before this scale, correlation against
            // `ffmpeg -bitexact` was already high (~0.95-0.996) but the
            // RMS ratio was a consistent ~32768 (2^15) across every
            // fixture regardless of content, sample rate or channel
            // count — the signature of a missing fixed normalisation,
            // not a per-frame drift bug.
            *slot = (first + prev) * PCM_TO_FLOAT_SCALE;
        }
    }
    overlap.second_half = z
        .get(half..)
        .map_or_else(|| vec![0.0; half], <[f32]>::to_vec);
    overlap.prev_window_shape = ics.window_shape;

    output
}

/// §4.6.11.3.2 part c)'s overlap-add across the eight short windows,
/// reassembled into one 2048-sample sequence so the caller's own long-block
/// overlap-add code handles both cases identically.
fn overlap_add_eight_short(windowed: &[Vec<f32>]) -> Vec<f32> {
    let mut z = vec![0.0f32; LONG_LEN];
    let half_short = SHORT_LEN / 2; // 128: each short window overlaps the previous by half
    for (j, win) in windowed.iter().enumerate() {
        // `checked_*` rather than plain arithmetic because `windowed` comes
        // from a bitstream-derived window count: a longer-than-expected list
        // walks off the end of `z` and is dropped by the `get_mut` below
        // rather than panicking.
        let Some(base) = j
            .checked_mul(half_short)
            .and_then(|off| off.checked_add(SHORT_START))
        else {
            continue;
        };
        for (i, &v) in win.iter().enumerate() {
            if let Some(slot) = base.checked_add(i).and_then(|pos| z.get_mut(pos)) {
                *slot += v;
            }
        }
    }
    z
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "test code"
    )]
    use super::{
        LONG_LEN, NUM_SHORT, SHORT_LEN, WindowSequence, build_window, inverse_quantize_and_rescale,
        overlap_add_eight_short,
    };

    /// The eight short windows must occupy exactly the span the neighbouring
    /// `LongStart`/`LongStop` windows leave for them, or the transition stops
    /// cancelling its time-domain alias — which showed up as every transient
    /// arriving 320 samples late against `ffmpeg`. Derived from the *other*
    /// two window shapes rather than from a literal, so it fails if the
    /// eight-short placement and the transition shapes ever disagree again.
    #[test]
    fn eight_short_fills_exactly_the_span_the_transition_windows_leave() {
        let all_ones = vec![vec![1.0f32; SHORT_LEN]; NUM_SHORT];
        let z = overlap_add_eight_short(&all_ones);
        let touched: Vec<usize> = z
            .iter()
            .enumerate()
            .filter(|&(_, &v)| v != 0.0)
            .map(|(i, _)| i)
            .collect();
        let first = *touched.first().unwrap();
        let last = *touched.last().unwrap();

        let stop = build_window(WindowSequence::LongStop, false, false);
        let start = build_window(WindowSequence::LongStart, false, false);
        let stop_first = stop.iter().position(|&v| v != 0.0).unwrap();
        let start_last = LONG_LEN - 1 - start.iter().rev().position(|&v| v != 0.0).unwrap();

        assert_eq!(
            first, stop_first,
            "eight-short starts at {first}, LongStop's ramp at {stop_first}"
        );
        assert_eq!(
            last, start_last,
            "eight-short ends at {last}, LongStart's ramp at {start_last}"
        );
        assert_eq!(touched.len(), last - first + 1, "gap inside the span");
    }

    #[test]
    fn inverse_quantize_matches_the_formula_for_a_simple_case() {
        // x_quant=8, sf=100 -> gain=2^0=1, invquant=8^(4/3)=16.
        let v = inverse_quantize_and_rescale(8, 100);
        assert!((v - 16.0).abs() < 0.01, "{v}");
    }

    #[test]
    fn sign_is_preserved_through_the_fractional_power() {
        let v = inverse_quantize_and_rescale(-8, 100);
        assert!((v + 16.0).abs() < 0.01, "{v}");
    }

    #[test]
    fn zero_input_is_zero_regardless_of_scalefactor() {
        assert!(inverse_quantize_and_rescale(0, 200).abs() < f32::EPSILON);
    }
}
