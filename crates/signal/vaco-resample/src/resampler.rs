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
use crate::dither::Dither;
use crate::mix::{MixLevels, MixMatrix, Rematrix, build_matrix};
use crate::opts::ResampleOptions;
use crate::rate::{RateConvert, RateParams};

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

        if !needs_mix && !needs_rate && dither.is_none() {
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
        Ok(Self {
            rematrix,
            rate,
            dither,
            rematrix_first,
            in_channels,
            out_channels,
            read: vec![Vec::new(); in_channels],
            mid: vec![Vec::new(); rate_channels.max(in_channels).max(out_channels)],
            pending: vec![Vec::new(); out_channels],
            dither_pos: 0,
            drained: 0,
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
        self.dither_pos = 0;
        self.drained = 0;
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
        from_rate.saturating_add(self.pending_len())
    }

    fn convert(
        &mut self,
        input: Option<AudioRef<'_>>,
        output: &mut AudioMut<'_>,
    ) -> Result<usize, Error> {
        let before = self.pending.first().map_or(0, Vec::len);
        match input {
            Some(src) => {
                let n = src.samples();
                if n > 0 {
                    for v in &mut self.read {
                        v.clear();
                    }
                    convert::read_planes::<T>(src, &mut self.read, n)?;
                    self.push(n)?;
                }
            }
            None => self.flush()?,
        }
        let after = self.pending.first().map_or(0, Vec::len);
        if after > before {
            self.dither_new(before, after - before);
        }
        self.drain(output)
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
        for (ch, plane) in self.pending.iter_mut().enumerate() {
            d.apply(plane, from, count, ch as u32, pos);
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
