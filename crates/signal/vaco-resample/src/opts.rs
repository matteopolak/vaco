//! The option surface, with the reference's names and aliases.
//!
//! # Why this is hand-written rather than `#[derive(Options)]`
//!
//! `vaco-opts` is the project's option mechanism and this struct should
//! eventually use it. It does not yet, for one concrete reason: the reference's
//! `SwrContext` option table includes `isf`/`osf`/`tsf` (sample formats) and
//! `ichl`/`ochl`/`uchl` (channel layouts), and `vaco-opts` names those bases
//! through an `OptValue` impl that "layer-1 crates implement" — but neither
//! `vaco-sampfmt` nor `vaco-chlayout` implements it today, and adding it is a
//! change to a crate this one does not own.
//!
//! Rather than reach across, the endpoint format and layout live in
//! [`AudioSpec`](crate::AudioSpec) where they are typed anyway, and this struct
//! carries only the scalars and enums. Migrating it to the derive is a
//! follow-up that needs those two impls first.
//!
//! # Divergences from the reference's defaults
//!
//! * `exact_rational` defaults to `true`, as the reference does — confirmed
//!   because `44100 → 48000` produces exactly 48000 output samples for 44100
//!   input samples, which only an exact-rational advance can do.
//! * `rematrix_maxval` default `0.0` means "derive from the output format":
//!   `1.0` for integer output, no ceiling for float. Measured; see [`crate::mix`].
//! * `resampler = soxr` is accepted, warned about, and aliased to the native
//!   engine (§B.13.3). Silent divergence is the one option we never take.

use vaco_core::Error;

use crate::design::Window;
use crate::dither::DitherMethod;
use crate::mix::MatrixEncoding;

/// Which resampling engine. `Soxr` is accepted and aliased.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Engine {
    #[default]
    Swr,
    Soxr,
}

/// The `filter_type` option surface, spelled as the reference spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FilterType {
    Cubic,
    BlackmanNuttall,
    #[default]
    Kaiser,
}

impl FilterType {
    #[must_use]
    pub const fn window(self) -> Window {
        match self {
            Self::Cubic => Window::Cubic,
            Self::BlackmanNuttall => Window::BlackmanNuttall,
            Self::Kaiser => Window::Kaiser,
        }
    }
}

/// Everything that is not an endpoint.
///
/// Six of the fields are booleans. That is not a design smell to be refactored
/// into a bitflags type: each one is a separately named option on the
/// reference's command line (`linear_interp`, `exact_rational`, `cheby`, …) and
/// collapsing them would make the mapping from an option name to a field
/// indirect for no gain.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one field per reference option name"
)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResampleOptions {
    // ── mixing ──────────────────────────────────────────────────────────────
    pub center_mix_level: f32,
    pub surround_mix_level: f32,
    pub lfe_mix_level: f32,
    pub rematrix_volume: f32,
    /// `0.0` means "derive from the output format".
    pub rematrix_maxval: f32,
    pub matrix_encoding: MatrixEncoding,

    // ── engine ──────────────────────────────────────────────────────────────
    pub engine: Engine,
    pub filter_size: i32,
    pub phase_shift: i32,
    pub linear_interp: bool,
    pub exact_rational: bool,
    /// `0.0` means "use the measured default of 0.97".
    pub cutoff: f64,
    pub filter_type: FilterType,
    pub kaiser_beta: f64,
    /// Accepted for `soxr` compatibility; ignored.
    pub precision: f64,
    /// Accepted for `soxr` compatibility; ignored.
    pub cheby: bool,
    /// Force resampling even when the rates match.
    pub force_resample: bool,

    // ── dither ──────────────────────────────────────────────────────────────
    pub dither_method: DitherMethod,
    pub dither_scale: f64,
    pub output_sample_bits: i32,

    // ── timestamp compensation ──────────────────────────────────────────────
    /// Seconds of drift below which no compensation, soft or hard, is applied.
    /// `f32::MAX` (the reference's `FLT_MAX` default) disables compensation
    /// entirely, since no real drift ever exceeds it.
    pub min_comp: f32,
    /// Seconds of drift above which the whole discrepancy is corrected in one
    /// step (padding or trimming), rather than spread out.
    pub min_hard_comp: f32,
    /// Seconds over which a soft correction is spread.
    pub comp_duration: f32,
    /// Cap, in samples per second, on how much a soft correction may stretch
    /// or squeeze the output. `0.0` disables soft compensation.
    pub max_soft_comp: f32,
    /// The reference's one-parameter convenience: `0` disables compensation,
    /// `1` enables hard-only (fill/trim) compensation, and `|async| > 1` also
    /// enables soft compensation capped at that many samples/sec. See
    /// [`ResampleOptions::effective_compensation`].
    pub async_samples: f32,
    /// Assume the first input sample is at this pts (in input-rate samples).
    /// `i64::MIN` (the reference's `AV_NOPTS_VALUE`-shaped sentinel) means
    /// "unset": the baseline is taken from whatever pts is first observed,
    /// so a stream's real (nonzero) start does not itself read as drift.
    pub first_pts: i64,

    // ── vaco extensions ─────────────────────────────────────────────────────
    pub dither_seed: u64,
}

impl Default for ResampleOptions {
    fn default() -> Self {
        Self {
            center_mix_level: core::f32::consts::FRAC_1_SQRT_2,
            surround_mix_level: core::f32::consts::FRAC_1_SQRT_2,
            lfe_mix_level: 0.0,
            rematrix_volume: 1.0,
            rematrix_maxval: 0.0,
            matrix_encoding: MatrixEncoding::None,
            engine: Engine::Swr,
            filter_size: 32,
            phase_shift: 10,
            linear_interp: false,
            exact_rational: true,
            cutoff: 0.0,
            filter_type: FilterType::Kaiser,
            kaiser_beta: 9.0,
            precision: 20.0,
            cheby: false,
            force_resample: false,
            dither_method: DitherMethod::None,
            dither_scale: 1.0,
            output_sample_bits: 0,
            min_comp: f32::MAX,
            min_hard_comp: 0.1,
            comp_duration: 1.0,
            max_soft_comp: 0.0,
            async_samples: 0.0,
            first_pts: i64::MIN,
            dither_seed: 0,
        }
    }
}

impl ResampleOptions {
    /// The effective cutoff: the option if set, otherwise the measured default.
    #[must_use]
    pub fn effective_cutoff(&self) -> f64 {
        if self.cutoff > 0.0 {
            self.cutoff
        } else {
            crate::design::DEFAULT_CUTOFF
        }
    }

    /// `first_pts`, or `None` for the reference's `AV_NOPTS_VALUE`-shaped
    /// sentinel meaning "unset".
    #[must_use]
    pub const fn first_pts(&self) -> Option<i64> {
        if self.first_pts == i64::MIN {
            None
        } else {
            Some(self.first_pts)
        }
    }

    /// Whether any timestamp-compensation option was touched away from its
    /// all-disabled default. Used to decide whether a resampler needs the
    /// dsp pipeline even when formats, rates and channel layouts otherwise
    /// allow the direct path — see `Resampler::new`.
    #[must_use]
    pub fn compensation_requested(&self) -> bool {
        self.async_samples != 0.0 || self.first_pts != i64::MIN || self.min_comp < f32::MAX
    }

    /// `async` folded into `min_comp` / `max_soft_comp`, per the measurements
    /// in `crate::timestamp`.
    #[must_use]
    pub(crate) fn effective_compensation(&self) -> crate::timestamp::Policy {
        let (min_comp, max_soft_comp) = if self.async_samples == 0.0 {
            (self.min_comp, self.max_soft_comp)
        } else {
            let soft = if self.async_samples.abs() > 1.0 {
                self.async_samples
            } else {
                0.0
            };
            (0.0, soft)
        };
        crate::timestamp::Policy {
            min_comp_s: f64::from(min_comp),
            min_hard_comp_s: f64::from(self.min_hard_comp),
            comp_duration_s: f64::from(self.comp_duration).max(f64::from(f32::EPSILON)),
            max_soft_comp: f64::from(max_soft_comp),
        }
    }

    /// Apply one `name=value` pair, using the reference's spelling.
    ///
    /// # Errors
    /// [`Error::Option`] for an unknown name or an unparseable value.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per option name is the clearest form for a compatibility table"
    )]
    pub fn set(&mut self, name: &str, value: &str) -> Result<(), Error> {
        let bad = |detail: String| Error::Option {
            name: name.to_owned(),
            detail,
        };
        let f32v = || {
            value
                .parse::<f32>()
                .map_err(|e| bad(format!("expected a float: {e}")))
        };
        let f64v = || {
            value
                .parse::<f64>()
                .map_err(|e| bad(format!("expected a float: {e}")))
        };
        let i32v = || {
            value
                .parse::<i32>()
                .map_err(|e| bad(format!("expected an integer: {e}")))
        };
        let boolv = || match value {
            "1" | "true" | "yes" | "on" => Ok(true),
            "0" | "false" | "no" | "off" => Ok(false),
            _ => Err(bad("expected a boolean".to_owned())),
        };
        match name {
            "clev" | "center_mix_level" => self.center_mix_level = f32v()?,
            "slev" | "surround_mix_level" => self.surround_mix_level = f32v()?,
            "lfe_mix_level" => self.lfe_mix_level = f32v()?,
            "rmvol" | "rematrix_volume" => self.rematrix_volume = f32v()?,
            "rematrix_maxval" => self.rematrix_maxval = f32v()?,
            "matrix_encoding" => {
                self.matrix_encoding = match value {
                    "none" => MatrixEncoding::None,
                    "dolby" => MatrixEncoding::Dolby,
                    "dplii" => MatrixEncoding::Dplii,
                    "dplii_x" => MatrixEncoding::DpliiX,
                    "dplii_z" => MatrixEncoding::DpliiZ,
                    "dolby_ex" => MatrixEncoding::DolbyEx,
                    "dolby_headphone" => MatrixEncoding::DolbyHeadphone,
                    _ => return Err(bad(format!("unknown matrix encoding `{value}`"))),
                };
            }
            "resampler" => {
                self.engine = match value {
                    "swr" => Engine::Swr,
                    "soxr" => {
                        tracing::warn!(
                            "libsoxr is not available in this build; using the native resampler"
                        );
                        Engine::Soxr
                    }
                    _ => return Err(bad(format!("unknown resampler `{value}`"))),
                };
            }
            "flags" | "swr_flags" => {
                self.force_resample = value.split('+').any(|f| f == "res");
            }
            "filter_size" => self.filter_size = i32v()?,
            "phase_shift" => self.phase_shift = i32v()?,
            "linear_interp" => self.linear_interp = boolv()?,
            "exact_rational" => self.exact_rational = boolv()?,
            "cutoff" | "resample_cutoff" => self.cutoff = f64v()?,
            "filter_type" => {
                self.filter_type = match value {
                    "cubic" => FilterType::Cubic,
                    "blackman_nuttall" => FilterType::BlackmanNuttall,
                    "kaiser" => FilterType::Kaiser,
                    _ => return Err(bad(format!("unknown filter type `{value}`"))),
                };
            }
            "kaiser_beta" => self.kaiser_beta = f64v()?,
            "precision" => {
                self.precision = f64v()?;
                tracing::debug!("`precision` applies to the soxr engine only; ignored");
            }
            "cheby" => {
                self.cheby = boolv()?;
                tracing::debug!("`cheby` applies to the soxr engine only; ignored");
            }
            "dither_method" => self.dither_method = DitherMethod::from_name(value)?,
            "dither_scale" => self.dither_scale = f64v()?,
            "output_sample_bits" => self.output_sample_bits = i32v()?,
            "min_comp" => self.min_comp = f32v()?,
            "min_hard_comp" => self.min_hard_comp = f32v()?,
            "comp_duration" => self.comp_duration = f32v()?,
            "max_soft_comp" => self.max_soft_comp = f32v()?,
            "async" => self.async_samples = f32v()?,
            "first_pts" => {
                self.first_pts = value
                    .parse::<i64>()
                    .map_err(|e| bad(format!("expected an integer: {e}")))?;
            }
            "dither_seed" => {
                self.dither_seed = value
                    .parse::<u64>()
                    .map_err(|e| bad(format!("expected an integer: {e}")))?;
            }
            _ => return Err(bad("unknown option".to_owned())),
        }
        Ok(())
    }

    /// Apply a `k=v:k=v` option string, as `aresample=` takes it.
    ///
    /// # Errors
    /// [`Error::Option`] for a malformed pair or an unknown name.
    pub fn set_from_str(&mut self, spec: &str) -> Result<(), Error> {
        for pair in spec.split(':').filter(|s| !s.is_empty()) {
            let Some((k, v)) = pair.split_once('=') else {
                return Err(Error::Option {
                    name: pair.to_owned(),
                    detail: "expected name=value".to_owned(),
                });
            };
            self.set(k.trim(), v.trim())?;
        }
        Ok(())
    }

    /// Reject values that would make the engine degenerate.
    ///
    /// # Errors
    /// [`Error::InvalidData`] naming the offending option.
    pub fn validate(&self) -> Result<(), Error> {
        if !(0..=65536).contains(&self.filter_size) || self.filter_size == 0 {
            return Err(Error::InvalidData("filter_size must be in 1..=65536"));
        }
        if !(0..=24).contains(&self.phase_shift) {
            return Err(Error::InvalidData("phase_shift must be in 0..=24"));
        }
        if !(0.0..=1.0).contains(&self.cutoff) {
            return Err(Error::InvalidData("cutoff must be in 0..=1"));
        }
        if !(2.0..=16.0).contains(&self.kaiser_beta) {
            return Err(Error::InvalidData("kaiser_beta must be in 2..=16"));
        }
        if !self.dither_scale.is_finite() || self.dither_scale < 0.0 {
            return Err(Error::InvalidData("dither_scale must be finite and >= 0"));
        }
        if !(0..=64).contains(&self.output_sample_bits) {
            return Err(Error::InvalidData("output_sample_bits must be in 0..=64"));
        }
        if !self.min_comp.is_finite() || self.min_comp < 0.0 {
            return Err(Error::InvalidData("min_comp must be finite and >= 0"));
        }
        if !self.min_hard_comp.is_finite() || self.min_hard_comp < 0.0 {
            return Err(Error::InvalidData("min_hard_comp must be finite and >= 0"));
        }
        if !self.comp_duration.is_finite() || self.comp_duration < 0.0 {
            return Err(Error::InvalidData("comp_duration must be finite and >= 0"));
        }
        if !self.max_soft_comp.is_finite() {
            return Err(Error::InvalidData("max_soft_comp must be finite"));
        }
        if !self.async_samples.is_finite() {
            return Err(Error::InvalidData("async must be finite"));
        }
        for v in [
            self.center_mix_level,
            self.surround_mix_level,
            self.lfe_mix_level,
        ] {
            if !v.is_finite() || v.abs() > 32.0 {
                return Err(Error::InvalidData("mix level must be finite and <= 32"));
            }
        }
        if !self.rematrix_volume.is_finite() || self.rematrix_volume > 1000.0 {
            return Err(Error::InvalidData("rematrix_volume out of range"));
        }
        if !self.rematrix_maxval.is_finite() || !(0.0..=1000.0).contains(&self.rematrix_maxval) {
            return Err(Error::InvalidData("rematrix_maxval out of range"));
        }
        Ok(())
    }
}
