//! The composition: format conversion → rematrix → rate conversion → dither →
//! format conversion.
//!
//! # Stage ordering is chosen, not inherited
//!
//! Rematrixing runs on whichever side has fewer channels. Folding 5.1 to stereo
//! *before* the resampler means the expensive stage runs on two planes instead
//! of six; up-mixing reverses the argument, so the builder compares the two
//! counts and places the stage accordingly. A fused implementation makes this
//! awkward; a staged one makes it a boolean.
//!
//! # The direct path
//!
//! When there is no rematrix, no rate change and no dither, the whole operation
//! is a sample-format conversion and it goes straight through
//! [`convert::convert`](crate::convert::convert) without touching the internal
//! float format. That is not an optimisation — it is what keeps `s32 → s16`
//! exact, because the reference converts it with an arithmetic shift and any
//! trip through `f32` would lose bits.

#![allow(
    clippy::integer_division,
    reason = "channel counts and sample widths are non-zero by construction"
)]

use vaco_chlayout::ChannelLayout;
use vaco_core::Error;
use vaco_limits::Budget;
use vaco_sampfmt::SampleFmt;

use crate::buf::{AudioMut, AudioRef, AudioSpec};
use crate::convert::{self, Elem, Internal};
use crate::dither::{Dither, NoiseShapeState};
use crate::mix::{MixLevels, MixMatrix, Rematrix, build_matrix};
use crate::opts::ResampleOptions;
use crate::rate::{RateConvert, RateParams};
use crate::timestamp::{self, Decision, MAX_COMPENSATION_SAMPLES, Policy, SoftWindow, Tracker};

/// A configured conversion from one [`AudioSpec`] to another.
#[derive(Debug)]
pub struct Resampler {
    input: AudioSpec,
    output: AudioSpec,
    core: Core,
}

#[derive(Debug)]
enum Core {
    /// Format conversion only.
    Direct,
    F32(Box<Pipeline<f32>>),
    F64(Box<Pipeline<f64>>),
}

#[derive(Debug)]
struct Pipeline<T: Internal> {
    rematrix: Option<Rematrix>,
    rate: Option<RateConvert<T>>,
    dither: Option<Dither>,
    /// Per-channel error-feedback state, when `dither` is one of the seven
    /// noise-shaping methods. See `dither`'s module docs for why this is not
    /// folded into `Dither` itself.
    ns_state: Option<NoiseShapeState>,
    /// Rematrix before rate conversion (true when it reduces the channel count).
    rematrix_first: bool,
    in_channels: usize,
    out_channels: usize,

    read: Vec<Vec<T>>,
    mid: Vec<Vec<T>>,
    pending: Vec<Vec<T>>,
    /// Absolute output position of `pending[..][0]`, for the dither sequence.
    dither_pos: u64,
    drained: usize,

    // ── timestamp compensation (crate::timestamp) ───────────────────────────
    in_rate: u32,
    out_rate: u32,
    comp_policy: Policy,
    comp_tracker: Tracker,
    /// Input-rate samples of silence (positive) or real input samples to
    /// discard (negative) queued by the last hard-compensation decision,
    /// applied before the next real input block reaches the mixer/resampler.
    pending_hard: i64,
    /// An in-progress soft correction, in output-rate samples.
    soft: Option<SoftWindow>,
}

impl Resampler {
    /// Configure a conversion.
    ///
    /// # Errors
    /// [`Error::InvalidData`] for a degenerate spec or option value,
    /// [`Error::Unsupported`] for a matrix encoding we do not implement, and
    /// [`Error::LimitExceeded`] if the coefficient bank exceeds `budget`.
    pub fn new(
        input: &AudioSpec,
        output: &AudioSpec,
        opts: &ResampleOptions,
        budget: &mut Budget,
    ) -> Result<Self, Error> {
        opts.validate()?;
        budget.check_channels(u64::from(input.channels()))?;
        budget.check_channels(u64::from(output.channels()))?;
        budget.check_sample_rate(u64::from(input.sample_rate))?;
        budget.check_sample_rate(u64::from(output.sample_rate))?;

        let int_output = !output.format.is_float();
        let levels = MixLevels {
            center: opts.center_mix_level,
            surround: opts.surround_mix_level,
            lfe: opts.lfe_mix_level,
            rematrix_volume: opts.rematrix_volume,
            rematrix_maxval: opts.rematrix_maxval,
        };
        let matrix = build_matrix(
            &input.layout,
            &output.layout,
            &levels,
            opts.matrix_encoding,
            int_output,
        )?;
        let needs_mix = !is_identity(&matrix);
        let needs_rate = input.sample_rate != output.sample_rate || opts.force_resample;
        // Timestamp compensation inserts/drops/stretches sample data, so it
        // needs the dsp pipeline even at matching rates with no mixing or
        // dither. `compensation_requested` is the caller's explicit signal —
        // untouched defaults never force the pipeline, so the direct path
        // (§2.1's exactness guarantee) is unaffected for everyone else.
        let needs_compensation = opts.compensation_requested();
        let dither = if opts.dither_method == crate::DitherMethod::None || !int_output {
            None
        } else {
            let bits = if opts.output_sample_bits > 0 {
                opts.output_sample_bits as u32
            } else {
                output.format.bits_per_sample()
            };
            Some(Dither::new(
                opts.dither_method,
                bits,
                opts.dither_scale,
                opts.dither_seed,
            ))
        };

        if !needs_mix && !needs_rate && dither.is_none() && !needs_compensation {
            return Ok(Self {
                input: input.clone(),
                output: output.clone(),
                core: Core::Direct,
            });
        }

        let wide = internal_is_f64(input.format, output.format);
        let core = if wide {
            Core::F64(Box::new(Pipeline::<f64>::new(
                input, output, opts, matrix, needs_mix, needs_rate, dither, budget,
            )?))
        } else {
            Core::F32(Box::new(Pipeline::<f32>::new(
                input, output, opts, matrix, needs_mix, needs_rate, dither, budget,
            )?))
        };
        Ok(Self {
            input: input.clone(),
            output: output.clone(),
            core,
        })
    }

    #[must_use]
    pub const fn input_spec(&self) -> &AudioSpec {
        &self.input
    }

    #[must_use]
    pub const fn output_spec(&self) -> &AudioSpec {
        &self.output
    }

    /// The internal working type: `"f32"`, `"f64"` or `"none"` for the direct
    /// format-conversion path.
    #[must_use]
    pub const fn internal(&self) -> &'static str {
        match self.core {
            Core::Direct => "none",
            Core::F32(_) => "f32",
            Core::F64(_) => "f64",
        }
    }

    /// Convert. `input` of `None` drains the filter and flushes.
    ///
    /// Returns samples per channel written to `output`. Output not consumed by
    /// this call is held and returned by the next one, so a caller with a small
    /// output buffer just calls again with `None`.
    ///
    /// # Errors
    /// [`Error::InvalidData`] if a buffer's format or channel count does not
    /// match the spec this resampler was built for.
    pub fn convert(
        &mut self,
        input: Option<AudioRef<'_>>,
        output: &mut AudioMut<'_>,
    ) -> Result<usize, Error> {
        if let Some(src) = input.as_ref()
            && (src.format() != self.input.format || src.channels() != self.input.channels())
        {
            return Err(Error::InvalidData("input buffer does not match the spec"));
        }
        if output.format() != self.output.format || output.channels() != self.output.channels() {
            return Err(Error::InvalidData("output buffer does not match the spec"));
        }
        match &mut self.core {
            Core::Direct => match input {
                Some(src) => convert::convert(src, output),
                None => Ok(0),
            },
            Core::F32(p) => p.convert(input, output),
            Core::F64(p) => p.convert(input, output),
        }
    }

    /// Upper bound on the output samples `in_samples` of input can produce,
    /// given the current internal state.
    #[must_use]
    pub fn out_samples(&self, in_samples: usize) -> usize {
        match &self.core {
            Core::Direct => in_samples,
            Core::F32(p) => p.out_samples(in_samples),
            Core::F64(p) => p.out_samples(in_samples),
        }
    }

    /// Internal delay, expressed in units of `rate`.
    ///
    /// Pass the output rate for output samples, `1` for seconds rounded down,
    /// or a timebase denominator for timestamps. This is the `swr_get_delay`
    /// equivalent.
    #[must_use]
    pub fn delay(&self, rate: i64) -> i64 {
        let in_rate = i64::from(self.input.sample_rate).max(1);
        let held = match &self.core {
            Core::Direct => 0,
            Core::F32(p) => p.delay_in_samples(),
            Core::F64(p) => p.delay_in_samples(),
        };
        let held = i64::try_from(held).unwrap_or(i64::MAX);
        held.saturating_mul(rate) / in_rate
    }

    /// The output PTS matching `input_pts`, accounting for the internal delay.
    ///
    /// Both are in output-sample units.
    #[must_use]
    pub fn next_pts(&self, input_pts: i64) -> i64 {
        input_pts.saturating_sub(self.delay(i64::from(self.output.sample_rate)))
    }

    /// Discard all filter state and start a new stream.
    pub fn reset(&mut self) {
        match &mut self.core {
            Core::Direct => {}
            Core::F32(p) => p.reset(),
            Core::F64(p) => p.reset(),
        }
    }

    /// Feed the pts (in input-rate samples) that the *next* input chunk
    /// passed to [`Resampler::convert`] is expected to carry, and let the
    /// configured soft/hard/`async` policy decide what compensation, if any,
    /// to queue before that chunk is processed.
    ///
    /// This is the automatic side of timestamp compensation. Call it once
    /// per input chunk, immediately before `convert`. See
    /// `docs/signal/vaco-resample.md` for the measured thresholds behind the
    /// decision, and [`crate::timestamp`] for the rule itself.
    ///
    /// # Errors
    /// [`Error::Unsupported`] if this resampler has no dsp stage to
    /// compensate through: rates match, there is no mixing or dither, and no
    /// compensation option (`async`, `first_pts`, or a `min_comp` below its
    /// disabled default) was set at construction. Force the stage
    /// unconditionally with `flags=+res` if you need compensation on an
    /// otherwise-direct conversion.
    /// [`Error::LimitExceeded`] if the computed correction exceeds
    /// [`crate::timestamp::MAX_COMPENSATION_SAMPLES`].
    pub fn advance_pts(&mut self, input_pts: i64) -> Result<(), Error> {
        match &mut self.core {
            Core::Direct => Err(Self::no_pipeline()),
            Core::F32(p) => p.advance_pts(input_pts),
            Core::F64(p) => p.advance_pts(input_pts),
        }
    }

    /// The manual API: request that the next `compensation_distance` output
    /// samples absorb `sample_delta` extra (positive) or fewer (negative)
    /// samples, spread smoothly rather than as a single step. Equivalent in
    /// purpose to the reference's `swr_set_compensation` — see
    /// [`crate::timestamp`] for what "smoothly" means here: our own
    /// linear-interpolation stretch, not a transcription of the reference's
    /// internal mechanism, which its public contract does not expose.
    ///
    /// `compensation_distance == 0` applies the whole delta at the very next
    /// output sample.
    ///
    /// # Errors
    /// [`Error::Unsupported`] — see [`Resampler::advance_pts`].
    /// [`Error::LimitExceeded`] if `sample_delta` exceeds
    /// [`crate::timestamp::MAX_COMPENSATION_SAMPLES`].
    pub fn set_compensation(
        &mut self,
        sample_delta: i32,
        compensation_distance: u32,
    ) -> Result<(), Error> {
        match &mut self.core {
            Core::Direct => Err(Self::no_pipeline()),
            Core::F32(p) => p.set_compensation(sample_delta, compensation_distance),
            Core::F64(p) => p.set_compensation(sample_delta, compensation_distance),
        }
    }

    fn no_pipeline() -> Error {
        Error::Unsupported(
            "timestamp compensation needs the dsp pipeline; set async, first_pts or min_comp, \
             or force it with flags=+res",
        )
    }
}

fn is_identity(m: &MixMatrix) -> bool {
    if m.rows != m.cols {
        return false;
    }
    // Exact comparison is the point: an identity matrix comes out of the
    // builder as literal 1.0 and 0.0, and anything else must take the mixing
    // path rather than being waved through by a tolerance.
    #[allow(clippy::float_cmp, reason = "identity is exact or it is not identity")]
    (0..m.rows).all(|o| (0..m.cols).all(|i| m.get(o, i) - if o == i { 1.0 } else { 0.0 } == 0.0))
}

/// `f64` internally whenever an endpoint carries more than 24 significant bits.
///
/// `s32`, `s64` and `f64` all do. Anything narrower round-trips through `f32`
/// exactly, so the wider type would only cost bandwidth.
fn internal_is_f64(a: SampleFmt, b: SampleFmt) -> bool {
    Elem::of(a).precision_bits() > 24 || Elem::of(b).precision_bits() > 24
}

impl<T: Internal> Pipeline<T> {
    #[allow(
        clippy::too_many_arguments,
        reason = "one private constructor called from one place"
    )]
    fn new(
        input: &AudioSpec,
        output: &AudioSpec,
        opts: &ResampleOptions,
        matrix: MixMatrix,
        needs_mix: bool,
        needs_rate: bool,
        dither: Option<Dither>,
        budget: &mut Budget,
    ) -> Result<Self, Error> {
        let in_channels = input.channels() as usize;
        let out_channels = output.channels() as usize;
        let rematrix = needs_mix.then(|| Rematrix::new(matrix));
        // Run the mixer on whichever side has fewer channels: folding 5.1 to
        // stereo before the resampler is a 3x saving on the expensive stage.
        let rematrix_first = out_channels <= in_channels;
        let rate_channels = if rematrix.is_some() && rematrix_first {
            out_channels
        } else {
            in_channels
        };
        let rate = if needs_rate {
            let params = RateParams {
                in_rate: input.sample_rate,
                out_rate: output.sample_rate,
                filter_size: opts.filter_size.max(1) as usize,
                phase_shift: opts.phase_shift.max(0) as u32,
                cutoff: opts.effective_cutoff(),
                window: opts.filter_type.window(),
                kaiser_beta: opts.kaiser_beta,
                exact_rational: opts.exact_rational,
                linear_interp: opts.linear_interp,
            };
            Some(RateConvert::<T>::new(&params, rate_channels, budget)?)
        } else {
            None
        };
        let ns_state = dither
            .filter(Dither::is_noise_shaping)
            .map(|d| NoiseShapeState::new(out_channels, d.taps()));
        Ok(Self {
            rematrix,
            rate,
            dither,
            ns_state,
            rematrix_first,
            in_channels,
            out_channels,
            read: vec![Vec::new(); in_channels],
            mid: vec![Vec::new(); rate_channels.max(in_channels).max(out_channels)],
            pending: vec![Vec::new(); out_channels],
            dither_pos: 0,
            drained: 0,
            in_rate: input.sample_rate,
            out_rate: output.sample_rate,
            comp_policy: opts.effective_compensation(),
            comp_tracker: Tracker::new(opts.first_pts()),
            pending_hard: 0,
            soft: None,
        })
    }

    fn reset(&mut self) {
        if let Some(r) = &mut self.rate {
            r.reset();
        }
        for v in self
            .read
            .iter_mut()
            .chain(self.mid.iter_mut())
            .chain(self.pending.iter_mut())
        {
            v.clear();
        }
        self.comp_tracker.reset();
        self.pending_hard = 0;
        self.soft = None;
        self.dither_pos = 0;
        self.drained = 0;
        if let Some(ns) = &mut self.ns_state {
            ns.reset();
        }
    }

    fn delay_in_samples(&self) -> u64 {
        let held = self.rate.as_ref().map_or(0, RateConvert::delay_in_samples);
        held.saturating_add(self.pending_len() as u64)
    }

    fn pending_len(&self) -> usize {
        self.pending
            .first()
            .map_or(0, |p| p.len().saturating_sub(self.drained))
    }

    fn out_samples(&self, in_samples: usize) -> usize {
        let from_rate = self.rate.as_ref().map_or(in_samples, |r| {
            usize::try_from(r.out_samples(in_samples as u64)).unwrap_or(usize::MAX)
        });
        // Queued compensation can grow the next call's output beyond what the
        // real input alone would: hard compensation may still insert silence,
        // and an in-progress soft window may still be adding net samples. An
        // insert (positive) is the only direction that raises the bound; a
        // drop or a net-negative soft correction only lowers actual output,
        // which a caller sizing a buffer from this bound does not need to know.
        let hard_extra = usize::try_from(self.pending_hard.max(0)).unwrap_or(usize::MAX);
        let soft_extra = self.soft.map_or(0, |s| {
            usize::try_from(s.remaining_delta.max(0)).unwrap_or(usize::MAX)
        });
        from_rate
            .saturating_add(self.pending_len())
            .saturating_add(hard_extra)
            .saturating_add(soft_extra)
    }

    fn convert(
        &mut self,
        input: Option<AudioRef<'_>>,
        output: &mut AudioMut<'_>,
    ) -> Result<usize, Error> {
        let before = self.pending.first().map_or(0, Vec::len);
        match input {
            Some(src) => {
                // Hard compensation queued by `advance_pts` is applied here,
                // ahead of the real block it was measured against: silence
                // goes in first, and any surplus real samples are dropped
                // from the front of this one.
                self.apply_pending_hard_insert()?;
                let n = src.samples();
                if n > 0 {
                    for v in &mut self.read {
                        v.clear();
                    }
                    convert::read_planes::<T>(src, &mut self.read, n)?;
                    let n = self.apply_pending_hard_drop(n);
                    if n > 0 {
                        self.push(n)?;
                        self.comp_tracker.account(n as u64);
                    }
                }
            }
            None => self.flush()?,
        }
        // Soft compensation reshapes the tail this call just produced, before
        // dither positions are assigned against the final sample count.
        self.apply_soft(before);
        let after = self.pending.first().map_or(0, Vec::len);
        if after > before {
            self.dither_new(before, after - before);
        }
        self.drain(output)
    }

    /// Push `n` samples of silence through the mixer/resampler, as if they
    /// were real input. `self.read` is used as scratch and restored
    /// afterwards, since nothing outside one `convert` call depends on its
    /// contents surviving a call.
    fn push_silence(&mut self, n: usize) -> Result<(), Error> {
        if n == 0 {
            return Ok(());
        }
        let mut silence = vec![Vec::new(); self.in_channels];
        for v in &mut silence {
            for _ in 0..n {
                v.push(T::ZERO);
            }
        }
        core::mem::swap(&mut self.read, &mut silence);
        let result = self.push(n);
        core::mem::swap(&mut self.read, &mut silence);
        result
    }

    /// Insert queued hard-compensation silence, if any, before the next real
    /// block. Input samples to *drop* are handled once the real block has
    /// been read, by [`Pipeline::apply_pending_hard_drop`].
    fn apply_pending_hard_insert(&mut self) -> Result<(), Error> {
        if self.pending_hard <= 0 {
            return Ok(());
        }
        let n = usize::try_from(self.pending_hard).unwrap_or(usize::MAX);
        self.pending_hard = 0;
        self.push_silence(n)?;
        self.comp_tracker.account(n as u64);
        Ok(())
    }

    /// Discard up to the queued amount from the front of `self.read`, which
    /// holds `n` freshly-read real samples. Returns the number remaining to
    /// process this call.
    fn apply_pending_hard_drop(&mut self, n: usize) -> usize {
        if self.pending_hard >= 0 || n == 0 {
            return n;
        }
        let want_drop = usize::try_from(-self.pending_hard).unwrap_or(usize::MAX);
        let take = want_drop.min(n);
        if take > 0 {
            for v in &mut self.read {
                let t = take.min(v.len());
                v.drain(..t);
            }
            self.pending_hard += i64::try_from(take).unwrap_or(i64::MAX);
            self.comp_tracker.account(take as u64);
        }
        n - take
    }

    /// Stretch or squeeze the output tail this call just produced —
    /// `pending[..][before..]` — to absorb whatever share of an in-progress
    /// soft correction it can carry.
    fn apply_soft(&mut self, before: usize) {
        let Some(soft) = self.soft.as_mut() else {
            return;
        };
        let tail_len = self
            .pending
            .first()
            .map_or(0, |p| p.len().saturating_sub(before));
        if tail_len == 0 {
            return;
        }
        let (share_distance, share_delta) = soft.share(tail_len);
        let exhausted = soft.is_exhausted();
        if share_distance == 0 {
            if exhausted {
                self.soft = None;
            }
            return;
        }
        let share_distance_i = i64::try_from(share_distance).unwrap_or(i64::MAX);
        let target_len = usize::try_from(share_distance_i.saturating_add(share_delta)).unwrap_or(0);
        for plane in &mut self.pending {
            let at = before.min(plane.len());
            let full_tail = plane.split_off(at);
            let take = share_distance.min(full_tail.len());
            let (chunk, rest) = full_tail.split_at(take);
            let stretched = timestamp::linear_resample(chunk, target_len);
            plane.extend(stretched);
            plane.extend_from_slice(rest);
        }
        if exhausted {
            self.soft = None;
        }
    }

    /// The automatic side of timestamp compensation: see
    /// [`Resampler::advance_pts`].
    fn advance_pts(&mut self, input_pts: i64) -> Result<(), Error> {
        match self
            .comp_tracker
            .observe(&self.comp_policy, self.in_rate, input_pts)
        {
            Decision::None => Ok(()),
            Decision::Hard(delta) => {
                if delta.unsigned_abs() > MAX_COMPENSATION_SAMPLES.unsigned_abs() {
                    return Err(Error::LimitExceeded {
                        limit: "resample timestamp compensation",
                        requested: delta.unsigned_abs(),
                        cap: MAX_COMPENSATION_SAMPLES.unsigned_abs(),
                    });
                }
                self.pending_hard = self.pending_hard.saturating_add(delta);
                Ok(())
            }
            Decision::Soft(delta_in, duration_s) => {
                self.queue_soft_from_input_delta(delta_in, duration_s)
            }
        }
    }

    /// Restate an input-rate drift measurement as an output-rate soft
    /// correction, bounded by `max_soft_comp`. See [`Resampler::advance_pts`].
    fn queue_soft_from_input_delta(&mut self, delta_in: i64, duration_s: f64) -> Result<(), Error> {
        let in_rate = f64::from(self.in_rate.max(1));
        let out_rate = f64::from(self.out_rate.max(1));
        #[allow(
            clippy::cast_precision_loss,
            reason = "delta is bounded by MAX_COMPENSATION_SAMPLES, far below f64's exact range"
        )]
        let delta_out = (delta_in as f64) * out_rate / in_rate;
        let cap =
            (self.comp_policy.max_soft_comp.abs() * self.comp_policy.comp_duration_s).max(0.0);
        let delta_out = delta_out.clamp(-cap, cap).round();
        if delta_out == 0.0 || !delta_out.is_finite() {
            return Ok(());
        }
        let distance = (duration_s * out_rate).round();
        let distance = if distance.is_finite() && distance >= 1.0 {
            distance as u64
        } else {
            1
        };
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to +-cap, itself bounded by the option's own validated range"
        )]
        let delta_out_i = delta_out as i64;
        self.soft = Some(SoftWindow::new(delta_out_i, distance)?);
        Ok(())
    }

    /// The manual API: see [`Resampler::set_compensation`].
    fn set_compensation(
        &mut self,
        sample_delta: i32,
        compensation_distance: u32,
    ) -> Result<(), Error> {
        let distance = u64::from(compensation_distance).max(1);
        self.soft = Some(SoftWindow::new(i64::from(sample_delta), distance)?);
        Ok(())
    }

    /// One block of `n` input samples through the mixer and the resampler.
    fn push(&mut self, n: usize) -> Result<(), Error> {
        let mut mid = core::mem::take(&mut self.mid);
        for v in &mut mid {
            v.clear();
        }
        let result = self.stage(&mut mid, n, false);
        self.mid = mid;
        result
    }

    fn flush(&mut self) -> Result<(), Error> {
        let mut mid = core::mem::take(&mut self.mid);
        for v in &mut mid {
            v.clear();
        }
        let result = self.stage(&mut mid, 0, true);
        self.mid = mid;
        result
    }

    /// Both directions of the stage ordering, and both the streaming and the
    /// draining case, in one place so they cannot drift apart.
    fn stage(&mut self, mid: &mut [Vec<T>], n: usize, drain: bool) -> Result<(), Error> {
        let Self {
            rematrix,
            rate,
            rematrix_first,
            in_channels,
            out_channels,
            read,
            pending,
            ..
        } = self;
        match (rematrix.as_ref(), *rematrix_first) {
            // mix first: the resampler runs on the (fewer) output channels
            (Some(rm), true) => {
                let scratch = mid
                    .get_mut(..*out_channels)
                    .ok_or(Error::InvalidData("mid buffer too small"))?;
                let m = if drain {
                    0
                } else {
                    rm.apply(read, 0, n, scratch)?;
                    n
                };
                match rate {
                    Some(r) if drain => {
                        r.flush(pending)?;
                    }
                    Some(r) => {
                        r.process(scratch, 0, m, pending)?;
                    }
                    None => {
                        for (dst, src) in pending.iter_mut().zip(scratch.iter()) {
                            dst.extend_from_slice(src.get(..m).unwrap_or_default());
                        }
                    }
                }
            }
            // resample first, then mix up to the (more numerous) output channels
            (Some(rm), false) => {
                let scratch = mid
                    .get_mut(..*in_channels)
                    .ok_or(Error::InvalidData("mid buffer too small"))?;
                let m = match rate {
                    Some(r) if drain => r.flush(scratch)?,
                    Some(r) => r.process(read, 0, n, scratch)?,
                    None if drain => 0,
                    None => {
                        for (dst, src) in scratch.iter_mut().zip(read.iter()) {
                            dst.extend_from_slice(src.get(..n).unwrap_or_default());
                        }
                        n
                    }
                };
                if m > 0 {
                    rm.apply(scratch, 0, m, pending)?;
                }
            }
            (None, _) => match rate {
                Some(r) if drain => {
                    r.flush(pending)?;
                }
                Some(r) => {
                    r.process(read, 0, n, pending)?;
                }
                None if drain => {}
                None => {
                    for (dst, src) in pending.iter_mut().zip(read.iter()) {
                        dst.extend_from_slice(src.get(..n).unwrap_or_default());
                    }
                }
            },
        }
        Ok(())
    }

    fn dither_new(&mut self, from: usize, count: usize) {
        let Some(d) = self.dither else {
            return;
        };
        let pos = self.dither_pos;
        if let Some(ns) = &mut self.ns_state {
            for (ch, plane) in self.pending.iter_mut().enumerate() {
                if let Some(history) = ns.channel_mut(ch) {
                    d.apply_shaped(plane, from, count, ch as u32, pos, history);
                }
            }
        } else {
            for (ch, plane) in self.pending.iter_mut().enumerate() {
                d.apply(plane, from, count, ch as u32, pos);
            }
        }
        self.dither_pos = self.dither_pos.saturating_add(count as u64);
    }

    fn drain(&mut self, output: &mut AudioMut<'_>) -> Result<usize, Error> {
        let avail = self.pending_len();
        let n = avail.min(output.samples());
        if n > 0 {
            convert::write_planes::<T>(&self.pending, self.drained, output, 0, n)?;
            self.drained += n;
        }
        // Reclaim once the consumed prefix is worth reclaiming.
        if self.drained > 0 && self.drained == self.pending.first().map_or(0, Vec::len) {
            for p in &mut self.pending {
                p.clear();
            }
            self.drained = 0;
        } else if self.drained >= 4096 {
            let d = self.drained;
            for p in &mut self.pending {
                p.drain(..d.min(p.len()));
            }
            self.drained = 0;
        }
        Ok(n)
    }
}

/// Convenience: the layout a caller gets when they only know a channel count.
#[must_use]
pub fn default_layout(channels: u32) -> ChannelLayout {
    ChannelLayout::default_for(channels).unwrap_or_else(|| ChannelLayout::unspecified(channels))
}
