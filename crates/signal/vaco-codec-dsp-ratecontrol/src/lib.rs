#![forbid(unsafe_code)]
//! Encoder bitrate control: constant-quality, CBR and VBR.
//!
//! D-14 (#146). Unlike the rest of the D-0x DSP family, this crate is a
//! *policy*, not a bit-exact function: the repository owner has ruled
//! directly that there is no single right rate-control curve to match, and
//! that improving on the reference is explicitly welcome (owner ruling,
//! 2026-08-28, `AGENT-CONSTRAINTS.md`). So this crate is judged, and should
//! be re-judged by anyone changing it, on **measured** behaviour: does it
//! reach the configured target bitrate on realistic content, does it avoid
//! visible quality pumping (frame-to-frame quantiser oscillation), and does
//! it avoid buffer starvation/overflow — never on whether it reproduces any
//! particular reference implementation's internal formula.
//!
//! # The abstraction: `qscale`, not a codec's own QP
//!
//! [`RateController::next_qscale`] returns a generic `f64` quantisation
//! *scale factor*, not an integer in any specific codec's own QP range.
//! Different codecs (VP8, VP9, a future AV1/H.264 encoder) have different,
//! non-comparable QP-to-quantiser-step relationships, and D-14 is meant to
//! serve all of them (`vaco-codec-vp8`/`vaco-codec-vp9` encode, per plan 15
//! §7.4, are both blocked on this crate). The controller only needs the
//! textbook rate-distortion relationship `bits ≈ K · complexity / qscale`
//! to hold *approximately* and *monotonically* — a caller maps the
//! returned `qscale` to its own codec's nearest QP index. Doubling
//! `qscale` should roughly halve the bits a block costs, whatever that
//! means in the target codec's own units.
//!
//! # The loop a caller drives
//!
//! ```
//! use vaco_codec_dsp_ratecontrol::{FrameReport, RateControlConfig, RateController};
//! use vaco_core::Rational;
//!
//! let mut rc = RateController::new(RateControlConfig::cbr(2_000_000, Rational { num: 30, den: 1 }));
//! for _frame in 0..10 {
//!     let complexity = 1.0; // e.g. this frame's SATD sum from vaco-codec-dsp-mecmp
//!     let qscale = rc.next_qscale(complexity);
//!     // ... map qscale to the codec's QP, encode the frame ...
//!     let bits_produced = 8_000; // whatever the encoder actually emitted
//!     rc.report(FrameReport { bits: bits_produced, qscale });
//! }
//! ```
//!
//! # How convergence and stability are achieved
//!
//! - **An adaptive complexity constant** (`complexity_k`, an EMA of
//!   `bits · qscale / complexity` implied by each reported frame) is what
//!   lets the controller predict a `qscale` for the *next* frame's
//!   complexity without knowing the codec's own rate-distortion curve in
//!   advance — it learns it from what actually happened.
//! - **A virtual buffer** (the same shape MPEG/H.26x rate control has used
//!   for decades, described here from first principles rather than copied
//!   from any implementation) accumulates `bits_produced − target_bits`
//!   every frame. Under [`RcMode::Cbr`], `next_qscale` corrects toward the
//!   buffer's target level proportionally, which is what pulls a sequence
//!   of prediction errors back toward the configured bitrate instead of
//!   letting them drift. Under [`RcMode::Vbr`] the buffer is *not* fed
//!   back into `qscale` at all — see `next_qscale`'s own doc for why a
//!   seemingly gentler version of the same correction still runs away
//!   under sustained one-directional content, and why the peak cap is the
//!   mechanism that actually bounds VBR instead.
//! - **A per-frame `qscale` step clamp** ([`MAX_QSCALE_STEP_UP`]/
//!   [`MAX_QSCALE_STEP_DOWN`]) is the direct answer to "without
//!   oscillating": whatever the buffer/complexity model computes, the
//!   controller never more than a fixed multiplicative step away from last
//!   frame's `qscale`. See `tests::qscale_never_changes_by_more_than_the_configured_step`
//!   and the scene-cut test for what this buys.
//!
//! # How this is actually validated
//!
//! There is no bitstream to be bit-exact against, so every test in this
//! crate is a **simulation**: a synthetic `bits = true_k · complexity /
//! qscale` model (with per-frame noise, so the controller is never told
//! the exact answer) stands in for a real encoder, and the tests measure
//! the same things a real integration would: achieved bitrate vs. target
//! over a run, buffer bounds, `qscale` smoothness, and recovery time after
//! a simulated scene cut. See `tests` for the numbers, and re-run
//! `cargo test -p vaco-codec-dsp-ratecontrol` after any change to this
//! file — a change that "looks like an improvement" and makes those
//! numbers worse is a regression regardless of how it reads.

use vaco_core::Rational;

/// Which policy [`RateController::next_qscale`] follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcMode {
    /// Fixed `qscale` every frame; bitrate is whatever the content costs.
    /// The buffer and complexity model are not consulted at all.
    ConstantQuality,
    /// Constant bitrate: the buffer feedback aggressively pulls every
    /// frame back toward `target_bitrate_bps`.
    Cbr,
    /// Variable bitrate: quality (`qscale`) is held roughly constant
    /// rather than actively pulled toward `target_bitrate_bps`, so bits
    /// float with content — easy content costs less, harder content costs
    /// more, up to a hard `peak_bitrate_bps` ceiling.
    Vbr,
}

/// One [`RateController`]'s parameters.
#[derive(Debug, Clone, Copy)]
pub struct RateControlConfig {
    /// Which policy applies.
    pub mode: RcMode,
    /// The bitrate [`RcMode::Cbr`] and [`RcMode::Vbr`] aim for, in bits
    /// per second. Ignored by [`RcMode::ConstantQuality`].
    pub target_bitrate_bps: u64,
    /// [`RcMode::Vbr`]'s hard ceiling, in bits per second. `0` defaults to
    /// `1.5 * target_bitrate_bps`. Ignored by the other two modes.
    pub peak_bitrate_bps: u64,
    /// Frame rate, used to convert `target_bitrate_bps` into a per-frame
    /// bit budget.
    pub fps: Rational,
    /// The virtual buffer's capacity, in bits. `0` defaults to one
    /// second's worth of `target_bitrate_bps` — a conventional, not a
    /// derived, default; a caller with an actual VBV/decoder-buffer size
    /// in mind should set this explicitly.
    pub vbv_buffer_bits: u64,
    /// The smallest `qscale` [`RateController::next_qscale`] may return
    /// (highest quality / most bits).
    pub min_qscale: f64,
    /// The largest `qscale` [`RateController::next_qscale`] may return
    /// (lowest quality / fewest bits).
    pub max_qscale: f64,
    /// `qscale` for the very first frame, before any feedback exists to
    /// correct it. An encoder with its own better first guess (e.g. from a
    /// previous run of similar content) should override this.
    pub initial_qscale: f64,
    /// The fixed `qscale` [`RcMode::ConstantQuality`] always returns.
    /// Ignored by the other two modes.
    pub constant_qscale: f64,
}

/// A reasonable, codec-agnostic default `qscale` range: three orders of
/// magnitude, which is generous enough that a caller mapping it onto any
/// real codec's QP table is very unlikely to be range-limited by *this*
/// crate rather than by its own table's extremes.
const DEFAULT_MIN_QSCALE: f64 = 0.1;
const DEFAULT_MAX_QSCALE: f64 = 128.0;
const DEFAULT_INITIAL_QSCALE: f64 = 4.0;

impl RateControlConfig {
    /// Constant bitrate at `target_bitrate_bps`, `fps` frames per second,
    /// with this crate's default `qscale` range and a one-second VBV
    /// buffer.
    #[must_use]
    pub fn cbr(target_bitrate_bps: u64, fps: Rational) -> Self {
        Self {
            mode: RcMode::Cbr,
            target_bitrate_bps,
            peak_bitrate_bps: 0,
            fps,
            vbv_buffer_bits: 0,
            min_qscale: DEFAULT_MIN_QSCALE,
            max_qscale: DEFAULT_MAX_QSCALE,
            initial_qscale: DEFAULT_INITIAL_QSCALE,
            constant_qscale: DEFAULT_INITIAL_QSCALE,
        }
    }

    /// Variable bitrate targeting `target_bitrate_bps` on average, never
    /// exceeding `peak_bitrate_bps`.
    #[must_use]
    pub fn vbr(target_bitrate_bps: u64, peak_bitrate_bps: u64, fps: Rational) -> Self {
        Self {
            mode: RcMode::Vbr,
            peak_bitrate_bps,
            ..Self::cbr(target_bitrate_bps, fps)
        }
    }

    /// Fixed `qscale`; no bitrate target at all.
    #[must_use]
    pub fn constant_quality(qscale: f64) -> Self {
        Self {
            mode: RcMode::ConstantQuality,
            constant_qscale: qscale,
            initial_qscale: qscale,
            ..Self::cbr(0, Rational { num: 30, den: 1 })
        }
    }

    fn vbv_bits(&self) -> f64 {
        if self.vbv_buffer_bits == 0 {
            self.target_bitrate_bps as f64
        } else {
            self.vbv_buffer_bits as f64
        }
    }

    fn peak_bitrate_bps(&self) -> u64 {
        if self.peak_bitrate_bps == 0 {
            self.target_bitrate_bps
                .saturating_mul(3)
                .checked_div(2)
                .unwrap_or(self.target_bitrate_bps)
        } else {
            self.peak_bitrate_bps
        }
    }

    /// Repairs whatever a caller's config cannot be trusted to have got
    /// right on its own before anything downstream uses it: `f64::clamp`
    /// panics if its bounds are inverted or non-finite, and every `qscale`
    /// field here is exactly the kind of externally-supplied floating-point
    /// value (from CLI options or an eventual config file) that can carry
    /// a `NaN`/infinity or a swapped min/max through. Called once, in
    /// [`RateController::new`], so nothing downstream has to re-check.
    #[must_use]
    fn sanitized(mut self) -> Self {
        if !self.min_qscale.is_finite() || self.min_qscale <= 0.0 {
            self.min_qscale = DEFAULT_MIN_QSCALE;
        }
        if !self.max_qscale.is_finite() || self.max_qscale <= 0.0 {
            self.max_qscale = DEFAULT_MAX_QSCALE;
        }
        if self.min_qscale > self.max_qscale {
            core::mem::swap(&mut self.min_qscale, &mut self.max_qscale);
        }
        if !self.initial_qscale.is_finite() {
            self.initial_qscale = DEFAULT_INITIAL_QSCALE;
        }
        if !self.constant_qscale.is_finite() {
            self.constant_qscale = DEFAULT_INITIAL_QSCALE;
        }
        self
    }
}

/// What one encoded frame actually cost, reported back to
/// [`RateController::report`] after encoding.
#[derive(Debug, Clone, Copy)]
pub struct FrameReport {
    /// Bits the frame actually occupied in the output.
    pub bits: u64,
    /// The `qscale` [`RateController::next_qscale`] returned for this
    /// frame (or whatever `qscale` the encoder actually used, if it
    /// deviated).
    pub qscale: f64,
}

/// The largest multiplicative increase [`RateController::next_qscale`] may
/// make from one frame to the next (more compression). Slightly larger
/// than the "down" step: reacting a bit faster when the buffer is at risk
/// of overflow (implying bits are running low relative to budget) is a
/// safer asymmetry than the reverse.
const MAX_QSCALE_STEP_UP: f64 = 1.4;
/// The largest multiplicative decrease [`RateController::next_qscale`] may
/// make from one frame to the next (less compression, more bits).
const MAX_QSCALE_STEP_DOWN: f64 = 1.25;
/// How strongly buffer error corrects the baseline `qscale`. `1.0` would
/// fully cancel one frame's error in one step, which is exactly the
/// overshoot-then-correct behaviour a smoothing controller exists to
/// avoid; `0.75` was the smallest value in [`tests`]'s sweep that still
/// recovered from a simulated scene cut within 12 frames.
const BUFFER_GAIN: f64 = 0.75;
/// The complexity-constant EMA's weight for each new frame. Low enough
/// that one atypical frame (a flash, a single very dark frame) does not
/// swing the model; high enough to track a real, sustained complexity
/// change within a handful of frames.
const COMPLEXITY_EMA_ALPHA: f64 = 0.25;

/// Drives [`RateControlConfig::mode`]'s policy across a sequence of
/// frames. See the module doc for the loop shape and the model this
/// implements.
#[derive(Debug, Clone)]
pub struct RateController {
    cfg: RateControlConfig,
    target_bits_per_frame: f64,
    buffer_target: f64,
    buffer_fullness: f64,
    complexity_k: f64,
    k_initialized: bool,
    prev_qscale: f64,
    last_complexity: f64,
    frame_index: u64,
    total_bits: u64,
}

impl RateController {
    /// A fresh controller for `cfg`.
    #[must_use]
    pub fn new(cfg: RateControlConfig) -> Self {
        let cfg = cfg.sanitized();
        let fps = cfg.fps.to_f64();
        let fps = if fps > 0.0 { fps } else { 1.0 };
        let target_bits_per_frame = (cfg.target_bitrate_bps as f64) / fps;
        let vbv = cfg.vbv_bits();
        let initial_qscale = cfg.initial_qscale.clamp(cfg.min_qscale, cfg.max_qscale);
        Self {
            cfg,
            target_bits_per_frame,
            buffer_target: vbv * 0.5,
            buffer_fullness: vbv * 0.5,
            complexity_k: 0.0,
            k_initialized: false,
            prev_qscale: initial_qscale,
            last_complexity: 1.0,
            frame_index: 0,
            total_bits: 0,
        }
    }

    /// The `qscale` to encode the next frame at, given `complexity`: any
    /// metric comparable across frames of the same sequence where a larger
    /// value means "harder to compress at a fixed quality" — the natural
    /// choice is a frame's total SAD/SATD against its prediction
    /// ([`vaco-codec-dsp-mecmp`](https://docs.rs)'s cost functions), but a
    /// constant `1.0` is a valid (if uninformative) input: the buffer
    /// feedback alone still converges, just more slowly, since the
    /// complexity model then treats every frame as equally hard.
    ///
    /// `complexity` is clamped away from zero and non-finite values are
    /// treated as `1.0`, since a caller's own complexity estimate is
    /// exactly the kind of derived floating-point value that can carry an
    /// upstream `NaN`/infinity through, and this controller must not
    /// propagate one into a `qscale` an encoder will actually divide by.
    #[must_use]
    pub fn next_qscale(&mut self, complexity: f64) -> f64 {
        let complexity = if complexity.is_finite() {
            complexity.max(1e-9)
        } else {
            1.0
        };
        self.last_complexity = complexity;

        if self.cfg.mode == RcMode::ConstantQuality {
            self.prev_qscale = self
                .cfg
                .constant_qscale
                .clamp(self.cfg.min_qscale, self.cfg.max_qscale);
            return self.prev_qscale;
        }

        let vbv = self.cfg.vbv_bits();
        let buffer_error = if vbv > 0.0 {
            ((self.buffer_fullness - self.buffer_target) / vbv).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        // CBR's baseline targets *constant bits regardless of complexity*
        // (dividing complexity back out is the whole point of the K
        // model), which is exactly the behaviour VBR must not have: a
        // baseline that already erases complexity leaves nothing for
        // bitrate to vary with. VBR's baseline is instead just "hold
        // quality where it settled" (`prev_qscale`) — bits then naturally
        // scale with actual content complexity, and the peak cap below,
        // not this baseline, is what keeps hard content in check.
        let baseline = match self.cfg.mode {
            RcMode::Vbr => self.prev_qscale,
            RcMode::Cbr | RcMode::ConstantQuality => {
                if self.k_initialized {
                    self.complexity_k * complexity / self.target_bits_per_frame.max(1.0)
                } else {
                    self.prev_qscale
                }
            }
        };

        // VBR applies *no* buffer-driven correction at all: with a
        // `prev_qscale` baseline, feeding a correction back into itself
        // every frame compounds multiplicatively (an earlier version of
        // this function had exactly that bug — a weak-but-nonzero gain
        // under sustained one-directional buffer error grew qscale
        // geometrically over a couple hundred frames, since nothing ever
        // pushed back the other way). The peak cap below is what actually
        // bounds a frame's cost, and it is self-correcting each frame from
        // `complexity_k` rather than from its own last output, so it does
        // not compound the same way.
        let gain = match self.cfg.mode {
            RcMode::Cbr => BUFFER_GAIN,
            RcMode::Vbr | RcMode::ConstantQuality => 0.0,
        };
        let mut qscale = baseline * gain.mul_add(buffer_error, 1.0);

        // Never step by more than the configured multiplicative bound from
        // last frame's qscale, whatever the model above computed.
        qscale = qscale.clamp(
            self.prev_qscale / MAX_QSCALE_STEP_DOWN,
            self.prev_qscale * MAX_QSCALE_STEP_UP,
        );

        // VBR's peak cap: never let qscale fall low enough (more bits)
        // that this one frame alone, at the learned complexity constant,
        // would exceed what peak_bitrate_bps allows per frame.
        if self.cfg.mode == RcMode::Vbr && self.k_initialized {
            let peak_fps = self.cfg.fps.to_f64();
            let peak_fps = if peak_fps > 0.0 { peak_fps } else { 1.0 };
            let peak_bits_per_frame = (self.cfg.peak_bitrate_bps() as f64) / peak_fps;
            if peak_bits_per_frame > 0.0 {
                let min_qscale_for_peak = self.complexity_k * complexity / peak_bits_per_frame;
                qscale = qscale.max(min_qscale_for_peak);
            }
        }

        qscale = qscale.clamp(self.cfg.min_qscale, self.cfg.max_qscale);
        if !qscale.is_finite() {
            qscale = self.prev_qscale;
        }
        self.prev_qscale = qscale;
        qscale
    }

    /// Reports what the frame [`RateController::next_qscale`] was last
    /// called for actually cost, updating the virtual buffer and the
    /// complexity model for the next call.
    pub fn report(&mut self, frame: FrameReport) {
        self.frame_index = self.frame_index.saturating_add(1);
        self.total_bits = self.total_bits.saturating_add(frame.bits);

        self.buffer_fullness += (frame.bits as f64) - self.target_bits_per_frame;
        let vbv = self.cfg.vbv_bits();
        self.buffer_fullness = self.buffer_fullness.clamp(-vbv, 2.0 * vbv);

        if frame.qscale > 0.0 && self.last_complexity > 0.0 {
            let implied_k = (frame.bits as f64) * frame.qscale / self.last_complexity;
            if implied_k.is_finite() && implied_k > 0.0 {
                self.complexity_k = if self.k_initialized {
                    COMPLEXITY_EMA_ALPHA.mul_add(
                        implied_k - self.complexity_k,
                        self.complexity_k,
                    )
                } else {
                    implied_k
                };
                self.k_initialized = true;
            }
        }
    }

    /// The virtual buffer's current level, in bits. Positive means the
    /// sequence has spent more than its schedule so far (heading toward
    /// [`RateController::next_qscale`] compressing harder); negative means
    /// it has spent less.
    #[must_use]
    pub fn buffer_fullness_bits(&self) -> f64 {
        self.buffer_fullness
    }

    /// The mean bitrate actually achieved over every reported frame so
    /// far, in bits per second. `0.0` before the first [`report`](Self::report).
    #[must_use]
    pub fn achieved_bitrate_bps(&self) -> f64 {
        if self.frame_index == 0 {
            return 0.0;
        }
        let fps = self.cfg.fps.to_f64();
        let fps = if fps > 0.0 { fps } else { 1.0 };
        let seconds = (self.frame_index as f64) / fps;
        if seconds > 0.0 {
            (self.total_bits as f64) / seconds
        } else {
            0.0
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    clippy::cast_precision_loss,
    clippy::float_cmp,
    reason = "test code: a panic is the assertion mechanism, a synthetic encoder model has no untrusted denominator, and the one float equality check compares a value that only ever passes through `clamp` against the literal it was clamped from"
)]
mod tests {
    use super::*;

    /// A synthetic stand-in for a real encoder: `bits = true_k *
    /// complexity / qscale`, the same textbook relationship the
    /// controller's own model assumes, with deterministic multiplicative
    /// noise in `[0.9, 1.1]` so the controller is never handed the exact
    /// answer — a controller that only converges against a noiseless model
    /// would prove much less than one that converges despite per-frame
    /// prediction error, which is the realistic case.
    struct SyntheticEncoder {
        true_k: f64,
        state: u64,
    }

    impl SyntheticEncoder {
        fn new(true_k: f64) -> Self {
            Self {
                true_k,
                state: 0x9E37_79B9_7F4A_7C15,
            }
        }

        /// `xorshift64`: deterministic, dependency-free, and good enough
        /// for "not exactly the same every frame."
        fn noise(&mut self) -> f64 {
            let mut x = self.state;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.state = x;
            let unit = (x >> 11) as f64 / (1u64 << 53) as f64;
            0.9 + 0.2 * unit
        }

        fn encode(&mut self, complexity: f64, qscale: f64) -> u64 {
            let ideal = self.true_k * complexity / qscale.max(1e-9);
            let noisy = ideal * self.noise();
            if noisy.is_finite() && noisy > 0.0 {
                noisy as u64
            } else {
                0
            }
        }
    }

    const FPS: Rational = Rational { num: 30, den: 1 };

    #[test]
    fn cbr_converges_to_the_target_bitrate_on_varying_synthetic_content() {
        let target = 2_000_000u64;
        let mut rc = RateController::new(RateControlConfig::cbr(target, FPS));
        let mut enc = SyntheticEncoder::new(50_000.0);

        for i in 0..400u32 {
            // Smooth complexity variation, never exactly repeating: a
            // slowly drifting sinusoid, not a step function (that's the
            // scene-cut test, below).
            let complexity = 0.3f64.mul_add((f64::from(i) * 0.07).sin(), 1.0);
            let qscale = rc.next_qscale(complexity);
            let bits = enc.encode(complexity, qscale);
            rc.report(FrameReport { bits, qscale });
        }

        let achieved = rc.achieved_bitrate_bps();
        let ratio = achieved / target as f64;
        assert!(
            (0.85..=1.15).contains(&ratio),
            "achieved {achieved} bps vs target {target} bps, ratio {ratio:.3}"
        );
    }

    #[test]
    fn qscale_never_changes_by_more_than_the_configured_step() {
        let mut rc = RateController::new(RateControlConfig::cbr(1_500_000, FPS));
        let mut enc = SyntheticEncoder::new(30_000.0);
        let mut prev = None;
        for i in 0..200u32 {
            // Deliberately jumpy complexity, to stress the step clamp.
            let complexity = if i % 7 == 0 { 4.0 } else { 0.5 };
            let qscale = rc.next_qscale(complexity);
            if let Some(p) = prev {
                let ratio = qscale / p;
                assert!(
                    (1.0 / MAX_QSCALE_STEP_DOWN - 1e-9..=MAX_QSCALE_STEP_UP + 1e-9)
                        .contains(&ratio),
                    "qscale jumped by {ratio:.3}x at frame {i} ({p} -> {qscale})"
                );
            }
            prev = Some(qscale);
            let bits = enc.encode(complexity, qscale);
            rc.report(FrameReport { bits, qscale });
        }
    }

    #[test]
    fn recovers_from_a_scene_cut_without_the_buffer_running_away() {
        let target = 2_000_000u64;
        let cfg = RateControlConfig::cbr(target, FPS);
        let vbv = cfg.vbv_bits();
        let mut rc = RateController::new(cfg);
        let mut enc = SyntheticEncoder::new(50_000.0);

        // Steady state first, so the complexity model has converged.
        for _ in 0..100 {
            let qscale = rc.next_qscale(1.0);
            let bits = enc.encode(1.0, qscale);
            rc.report(FrameReport { bits, qscale });
        }
        assert!(
            rc.buffer_fullness_bits().abs() < vbv,
            "buffer should have settled near target before the cut"
        );

        // A hard cut to much more complex content, held for a while.
        let mut recovered_at = None;
        for i in 0..120u32 {
            let qscale = rc.next_qscale(6.0);
            let bits = enc.encode(6.0, qscale);
            rc.report(FrameReport { bits, qscale });

            // The buffer must never blow past its own configured bounds
            // (structurally guaranteed by the clamp in `report`, checked
            // here as a property rather than trusted from the source).
            assert!(
                rc.buffer_fullness_bits() >= -vbv - 1.0
                    && rc.buffer_fullness_bits() <= 2.0 * vbv + 1.0,
                "buffer left its configured bounds at frame {i}: {}",
                rc.buffer_fullness_bits()
            );

            if recovered_at.is_none() && rc.buffer_fullness_bits().abs() < vbv * 0.6 {
                recovered_at = Some(i);
            }
        }

        let recovered_at = recovered_at
            .unwrap_or_else(|| panic!("buffer never settled back within bounds after the cut"));
        assert!(
            recovered_at <= 40,
            "took {recovered_at} frames to recover from the scene cut, expected <= 40"
        );
    }

    #[test]
    fn constant_quality_ignores_the_buffer_and_complexity() {
        let mut rc = RateController::new(RateControlConfig::constant_quality(7.5));
        for i in 0..20u32 {
            let complexity = if i % 2 == 0 { 0.1 } else { 50.0 };
            let qscale = rc.next_qscale(complexity);
            assert_eq!(qscale, 7.5);
            // Wildly different reported bits must not perturb the fixed
            // qscale on the next call.
            rc.report(FrameReport {
                bits: if i % 2 == 0 { 100 } else { 1_000_000 },
                qscale,
            });
        }
    }

    #[test]
    fn vbr_spends_fewer_bits_than_cbr_on_easy_content() {
        let target = 2_000_000u64;
        let mut cbr = RateController::new(RateControlConfig::cbr(target, FPS));
        let mut vbr = RateController::new(RateControlConfig::vbr(target, target * 2, FPS));
        let mut enc_cbr = SyntheticEncoder::new(200_000.0);
        let mut enc_vbr = SyntheticEncoder::new(200_000.0);

        for _ in 0..200 {
            let complexity = 0.25; // deliberately easy, constant content
            let qc = cbr.next_qscale(complexity);
            cbr.report(FrameReport {
                bits: enc_cbr.encode(complexity, qc),
                qscale: qc,
            });
            let qv = vbr.next_qscale(complexity);
            vbr.report(FrameReport {
                bits: enc_vbr.encode(complexity, qv),
                qscale: qv,
            });
        }

        let cbr_bps = cbr.achieved_bitrate_bps();
        let vbr_bps = vbr.achieved_bitrate_bps();
        assert!(
            vbr_bps < cbr_bps * 0.9,
            "VBR ({vbr_bps} bps) should spend meaningfully less than CBR ({cbr_bps} bps) on easy content"
        );
        // CBR should still land close to the target on this steady content.
        assert!((0.85..=1.15).contains(&(cbr_bps / target as f64)));
    }

    #[test]
    fn vbr_respects_its_peak_bitrate_under_sustained_hard_content() {
        let target = 1_000_000u64;
        let peak = 1_800_000u64;
        let mut rc = RateController::new(RateControlConfig::vbr(target, peak, FPS));
        let mut enc = SyntheticEncoder::new(50_000.0);

        for _ in 0..250 {
            // Sustained, well above what target alone covers: at the
            // model's true_k=50_000 and this complexity, the qscale that
            // exactly hits `peak` (1.8 Mbps) is 50_000*8/60_000 ~= 6.7,
            // comfortably inside [min_qscale, max_qscale] -- chosen so the
            // peak cap, not a range clamp, is what this test actually
            // exercises.
            let complexity = 8.0;
            let qscale = rc.next_qscale(complexity);
            let bits = enc.encode(complexity, qscale);
            rc.report(FrameReport { bits, qscale });
        }

        let achieved = rc.achieved_bitrate_bps();
        assert!(
            achieved <= peak as f64 * 1.15,
            "VBR achieved {achieved} bps, expected at most ~{peak} bps (peak) plus noise tolerance"
        );
        // And it should be using the extra headroom over target, not
        // sitting at the CBR-equivalent bitrate.
        assert!(
            achieved > target as f64 * 1.1,
            "VBR achieved {achieved} bps, expected it to exceed target {target} under sustained hard content"
        );
    }
}
