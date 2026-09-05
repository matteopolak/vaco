//! The option surface, declared once through `vaco-opts`.
//!
//! Names and aliases are interface facts and are preserved verbatim (D1/D9),
//! including the ones we would not have chosen. Options this crate has not
//! implemented are still *accepted* — the CLI has to keep working — and are
//! reported by [`ScaleOptions::unimplemented`] so a caller can warn rather than
//! silently produce something else. Refusing an option that the reference
//! accepts is a worse failure than ignoring it, but ignoring it *silently* is
//! worse than both.

use vaco_opts::{OptEnum, Options, opt_flags};

use crate::filter::Kernel;

opt_flags! {
    /// legacy scaler algorithm and modifier flags
    #[unit = "sws_flags"]
    pub struct SwsFlags: u64 {
        /// nearest neighbour. ffmpeg's real AVOption name for `SWS_POINT` is
        /// `neighbor`, not `point` -- this constant's own Rust identifier
        /// predates that check and stayed `POINT`, but the *string* name a
        /// `sws_flags=`/`flags=` value is parsed against must match the
        /// reference verbatim (D1/D9), so the string is `neighbor`.
        const POINT         = 1 << 4 => "neighbor";
        /// bilinear
        const BILINEAR      = 1 << 0 => "bilinear";
        /// bicubic
        const BICUBIC       = 1 << 2 => "bicubic";
        /// experimental
        const X             = 1 << 3 => "experimental";
        /// weighted area averaging
        const AREA          = 1 << 5 => "area";
        /// luma bicubic, chroma bilinear
        const BICUBLIN      = 1 << 6 => "bicublin";
        /// gaussian
        const GAUSS         = 1 << 7 => "gauss";
        /// sinc
        const SINC          = 1 << 8 => "sinc";
        /// Lanczos
        const LANCZOS       = 1 << 9 => "lanczos";
        /// natural bicubic spline
        const SPLINE        = 1 << 10 => "spline";
        /// accurate rounding
        const ACCURATE_RND  = 1 << 18 => "accurate_rnd";
        /// force bit-exact output
        const BITEXACT      = 1 << 19 => "bitexact";
        /// full chroma interpolation
        const FULL_CHROMA_INT = 1 << 13 => "full_chroma_int";
        /// full chroma input
        const FULL_CHROMA_INP = 1 << 14 => "full_chroma_inp";
        /// print sws info
        const PRINT_INFO    = 1 << 12 => "print_info";
    }
}

/// New-style scaler selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "sws_scaler", base = "int")]
pub enum ScalerKind {
    /// Resolve to the crate default (bicubic).
    #[opt_const(name = "auto", help = "automatic selection")]
    #[default]
    Auto,
    /// Nearest neighbour.
    #[opt_const(name = "nearest", help = "nearest neighbour")]
    Nearest,
    /// Bilinear.
    #[opt_const(name = "bilinear", help = "bilinear")]
    Bilinear,
    /// Mitchell-Netravali cubic.
    #[opt_const(name = "bicubic", help = "2-tap cubic")]
    Bicubic,
    /// Gaussian.
    #[opt_const(name = "gaussian", help = "gaussian")]
    Gaussian,
    /// Lanczos.
    #[opt_const(name = "lanczos", help = "3-tap sinc/sinc")]
    Lanczos,
    /// Box / area average.
    #[opt_const(name = "area", help = "box averaging")]
    Area,
}

/// Dither method applied when the destination is shallower than the working
/// precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "sws_dither", base = "int")]
pub enum DitherKind {
    /// Resolve at plan time: `Bayer` for any depth reduction, else `None`.
    #[opt_const(name = "auto", help = "automatic selection")]
    #[default]
    Auto,
    /// Round to nearest with no noise shaping.
    #[opt_const(name = "none", help = "no dithering")]
    None,
    /// Ordered dither on a recursively generated 8x8 Bayer matrix.
    #[opt_const(name = "bayer", help = "ordered dither")]
    Bayer,
}

/// Rendering policy when the source exceeds the destination's dynamic range or gamut.
///
/// The four variants use the ICC vocabulary so callers can carry a rendering
/// intent through a scaler without translating it into an implementation-specific
/// name.  The colour stage resolves this once while it builds its LUT.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "intent", base = "int")]
pub enum RenderingIntent {
    /// Compress tones and chroma smoothly for photographic material.
    #[opt_const(name = "perceptual", help = "smooth tone and gamut compression")]
    Perceptual,
    /// Adapt white points and clip only colours outside the destination gamut.
    #[opt_const(name = "relative_colorimetric", help = "white-point adapted clipping")]
    #[default]
    RelativeColorimetric,
    /// Preserve chroma direction while mapping it onto the destination boundary.
    #[opt_const(name = "saturation", help = "preserve relative saturation")]
    Saturation,
    /// Preserve absolute white point and clip outside the destination gamut.
    #[opt_const(name = "absolute_colorimetric", help = "unadapted clipping")]
    AbsoluteColorimetric,
}

/// Every knob the scaler exposes.
///
/// Construct with `ScaleOptions::default()` and mutate, or parse a
/// `key=value:key=value` string with `vaco_opts::OptionsExt::set_from_string`.
#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "sws", help = "image scaling and pixel format conversion")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the reference's option table has five boolean knobs and their \
              names are an interface fact (D1); grouping them into an enum \
              would break `-h filter=scale`"
)]
pub struct ScaleOptions {
    /// Legacy algorithm bitmask. Normalised into `scaler`/`scaler_sub` at plan
    /// time when those are `Auto`.
    #[opt(
        name = "sws_flags",
        unit = "sws_flags",
        help = "legacy scaler algorithm and modifier flags",
        default = SwsFlags::empty(),
        default_repr = "0",
        flags(video, param)
    )]
    pub flags: SwsFlags,

    /// Luma scaling algorithm.
    #[opt(
        name = "scaler",
        unit = "sws_scaler",
        help = "luma scaling algorithm",
        default = ScalerKind::Auto,
        default_repr = "auto",
        flags(video, param)
    )]
    pub scaler: ScalerKind,

    /// Chroma (subsampled plane) scaling algorithm.
    #[opt(
        name = "scaler_sub",
        unit = "sws_scaler",
        help = "chroma scaling algorithm",
        default = ScalerKind::Auto,
        default_repr = "auto",
        flags(video, param)
    )]
    pub scaler_sub: ScalerKind,

    /// Kernel parameter 0: bicubic `B`, Lanczos `a`, gaussian `sigma`.
    #[opt(name = "param0", help = "scaler parameter 0", default = f64::NAN,
          default_repr = "nan", flags(video, param))]
    pub param0: f64,

    /// Kernel parameter 1: bicubic `C`.
    #[opt(name = "param1", help = "scaler parameter 1", default = f64::NAN,
          default_repr = "nan", flags(video, param))]
    pub param1: f64,

    /// Treat the source as full range regardless of its signalling.
    #[opt(
        name = "src_range",
        help = "source is full range",
        default = false,
        flags(video, param)
    )]
    pub src_range_full: bool,

    /// Treat the destination as full range regardless of its signalling.
    #[opt(
        name = "dst_range",
        help = "destination is full range",
        default = false,
        flags(video, param)
    )]
    pub dst_range_full: bool,

    /// Dither method for depth reduction.
    #[opt(
        name = "sws_dither",
        unit = "sws_dither",
        help = "dither method",
        default = DitherKind::Auto,
        default_repr = "auto",
        flags(video, param)
    )]
    pub dither: DitherKind,

    /// Rendering intent for HDR tone and wide-gamut mapping.
    #[opt(
        name = "intent",
        unit = "intent",
        help = "tone and gamut rendering intent",
        default = RenderingIntent::RelativeColorimetric,
        default_repr = "relative_colorimetric",
        flags(video, param)
    )]
    pub intent: RenderingIntent,

    /// Grid edge length for the cached tone/gamut 3D LUT.
    #[opt(
        name = "lut3d_size",
        help = "tone and gamut LUT grid size",
        default = 33,
        range = 9..=65,
        flags(video, param)
    )]
    pub lut3d_size: i32,

    /// Cap on the tap count of any single filter bank.
    #[opt(name = "max_taps", help = "maximum filter taps", default = 64,
          range = 1..=1024, flags(video, param))]
    pub max_taps: i32,

    /// Worker threads. `0` means "derive from the machine and the work".
    #[opt(name = "threads", help = "worker threads", default = 0,
          range = 0..=256, flags(video, param))]
    pub threads: i32,

    /// Horizontal source chroma position, in 1/256 of a chroma sample.
    /// `-513` means "unset", which is no shift.
    #[opt(name = "src_h_chr_pos", help = "source horizontal chroma position",
          default = -513, range = -513..=1024, flags(video, param))]
    pub src_h_chr_pos: i32,

    /// Vertical source chroma position, in 1/256 of a chroma sample.
    #[opt(name = "src_v_chr_pos", help = "source vertical chroma position",
          default = -513, range = -513..=1024, flags(video, param))]
    pub src_v_chr_pos: i32,

    /// Horizontal destination chroma position, in 1/256 of a chroma sample.
    #[opt(name = "dst_h_chr_pos", help = "destination horizontal chroma position",
          default = -513, range = -513..=1024, flags(video, param))]
    pub dst_h_chr_pos: i32,

    /// Vertical destination chroma position, in 1/256 of a chroma sample.
    #[opt(name = "dst_v_chr_pos", help = "destination vertical chroma position",
          default = -513, range = -513..=1024, flags(video, param))]
    pub dst_v_chr_pos: i32,

    /// Scale in linear light. Accepted, not implemented.
    #[opt(
        name = "gamma",
        help = "gamma-correct scaling",
        default = false,
        flags(video, param)
    )]
    pub gamma: bool,

    /// Force bit-exact output. Already the default for every integer path.
    #[opt(
        name = "bitexact",
        help = "force bit-exact output",
        default = false,
        flags(video, param)
    )]
    pub bitexact: bool,
}

impl ScaleOptions {
    /// Parse a filtergraph-style `key=value:key=value` string.
    ///
    /// A thin wrapper over `vaco_opts::OptionsExt::set_from_string`, so callers
    /// — including the fuzz target — do not have to name `vaco-opts` themselves.
    ///
    /// # Errors
    ///
    /// [`vaco_core::Error::Option`] naming the first option that was not
    /// recognised or did not parse.
    pub fn parse(&mut self, s: &str) -> vaco_core::Result<()> {
        use vaco_opts::OptionsExt as _;
        self.set_from_string(s, "=", ":")
            .map_err(|e| vaco_core::Error::Option {
                name: "sws".to_owned(),
                detail: e.to_string(),
            })
    }

    /// Options this crate accepts but does not act on, so a caller can warn.
    ///
    /// Empty for a default option set.
    #[must_use]
    pub fn unimplemented(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.gamma {
            out.push("gamma");
        }
        if self.flags.contains(SwsFlags::SINC) {
            out.push("sws_flags=sinc");
        }
        if self.flags.contains(SwsFlags::SPLINE) {
            out.push("sws_flags=spline");
        }
        out
    }

    /// The luma kernel this option set selects.
    #[must_use]
    pub fn luma_kernel(&self) -> Kernel {
        self.kernel_for(self.resolved_luma())
    }

    /// The chroma kernel this option set selects.
    #[must_use]
    pub fn chroma_kernel(&self) -> Kernel {
        self.kernel_for(self.resolved_chroma())
    }

    fn resolved_luma(&self) -> ScalerKind {
        if self.scaler != ScalerKind::Auto {
            return self.scaler;
        }
        from_flags(self.flags).0
    }

    fn resolved_chroma(&self) -> ScalerKind {
        if self.scaler_sub != ScalerKind::Auto {
            return self.scaler_sub;
        }
        if self.scaler != ScalerKind::Auto {
            return self.scaler;
        }
        from_flags(self.flags).1
    }

    fn kernel_for(&self, kind: ScalerKind) -> Kernel {
        let p0 = self.param0;
        let p1 = self.param1;
        match kind {
            ScalerKind::Nearest => Kernel::Point,
            ScalerKind::Bilinear => Kernel::Bilinear,
            ScalerKind::Area => Kernel::Area,
            ScalerKind::Gaussian => Kernel::Gaussian {
                sigma: if p0.is_finite() && p0 > 0.0 { p0 } else { 1.0 },
            },
            ScalerKind::Lanczos => Kernel::Lanczos {
                a: if p0.is_finite() && p0 >= 1.0 { p0 } else { 3.0 },
            },
            ScalerKind::Auto | ScalerKind::Bicubic => {
                let d = Kernel::bicubic_default();
                let Kernel::Bicubic { b, c } = d else {
                    return d;
                };
                Kernel::Bicubic {
                    b: if in_bicubic_range(p0) { p0 } else { b },
                    c: if in_bicubic_range(p1) { p1 } else { c },
                }
            }
        }
    }
}

/// Whether `v` is a sane Mitchell-Netravali `B`/`C`, not merely finite.
///
/// `filter::mitchell`'s polynomial terms scale linearly with `B` and `C`
/// (the widest, at `ax` near 2, is roughly `(8B + 24C) / 6`), and every named
/// member of the cubic family — Catmull-Rom, Mitchell, B-spline, this
/// crate's own measured `(0, 0.6)` default — uses `B, C` in `0..=1`. `[-4, 4]`
/// is a generous multiple of that with room for legitimate experimentation,
/// while still keeping the largest possible kernel weight (on the order of
/// 20, before the `COEFF_SHIFT` fixed-point scale) far inside the `i32`
/// coefficient's designed headroom of "8-bit input at up to 16 taps"
/// (`filter.rs`'s doc comment on `COEFF_SHIFT`).
///
/// A `param0`/`param1` outside this range used to reach `mitchell()` as-is:
/// at `B = 1_000_000, C = 9_551_615` (reachable from an ordinary option
/// string, `sws_flags=param0=1000000:param1=9551615`) the polynomial's raw
/// output is itself on the order of `10^7`, which the fixed-point cast to
/// `i32` silently saturates to `i32::MAX`/`i32::MIN` rather than erroring —
/// and that saturated "coefficient", multiplied against a real pixel value
/// and accumulated in `exec.rs`'s `filter_v`, is what overflowed the `i64`
/// accumulator there. Silently falling back to the built-in default here
/// (exactly what already happens for a non-finite `param0`/`param1`, just
/// extended to cover "finite but nonsensical") is the fix, not widening
/// that accumulator further: the coefficient itself was already numerically
/// meaningless by the time it reached `filter_v`, and letting a wider
/// integer type "absorb" a meaningless value would trade a panic for a
/// silently wrong picture.
fn in_bicubic_range(v: f64) -> bool {
    v.is_finite() && (-4.0..=4.0).contains(&v)
}

/// `(luma, chroma)` selection implied by the legacy bitmask.
///
/// `bicublin` is the legacy spelling of "bicubic luma, bilinear chroma" and is
/// normalised here, so the planner never sees it.
fn from_flags(f: SwsFlags) -> (ScalerKind, ScalerKind) {
    if f.contains(SwsFlags::BICUBLIN) {
        return (ScalerKind::Bicubic, ScalerKind::Bilinear);
    }
    for (bit, kind) in [
        (SwsFlags::POINT, ScalerKind::Nearest),
        (SwsFlags::BILINEAR, ScalerKind::Bilinear),
        (SwsFlags::BICUBIC, ScalerKind::Bicubic),
        (SwsFlags::AREA, ScalerKind::Area),
        (SwsFlags::GAUSS, ScalerKind::Gaussian),
        (SwsFlags::LANCZOS, ScalerKind::Lanczos),
        // `sinc` and `spline` are accepted and fall back to bicubic; see
        // `unimplemented()`.
        (SwsFlags::SINC, ScalerKind::Bicubic),
        (SwsFlags::SPLINE, ScalerKind::Bicubic),
    ] {
        if f.contains(bit) {
            return (kind, kind);
        }
    }
    (ScalerKind::Bicubic, ScalerKind::Bicubic)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::many_single_char_names,
    clippy::float_cmp,
    clippy::integer_division,
    clippy::needless_range_loop,
    clippy::field_reassign_with_default,
    clippy::unreadable_literal,
    clippy::cast_possible_wrap,
    reason = "a failing assertion in a test is a failing test"
)]
mod tests {
    use super::*;
    use vaco_opts::OptionsExt;

    #[test]
    fn default_is_bicubic_with_the_measured_parameters() {
        let o = ScaleOptions::default();
        assert_eq!(o.luma_kernel(), Kernel::Bicubic { b: 0.0, c: 0.6 });
        assert_eq!(o.chroma_kernel(), Kernel::Bicubic { b: 0.0, c: 0.6 });
        assert!(o.unimplemented().is_empty());
    }

    /// A `param0`/`param1` this far out of range used to reach `mitchell()`
    /// unclamped: its raw output there is on the order of `10^7`, which the
    /// fixed-point cast to `i32` silently saturates rather than errors on,
    /// and that saturated coefficient overflowed `exec.rs`'s `filter_v`
    /// accumulator on the very next multiply-accumulate. This is a
    /// parameter-validation bug, not an accumulator-width one: the fix is
    /// falling back to the built-in default exactly as an already-non-finite
    /// `param0`/`param1` does, not widening the arithmetic to tolerate a
    /// coefficient that was never numerically meaningful in the first place.
    #[test]
    fn a_wildly_out_of_range_bicubic_parameter_falls_back_to_the_default() {
        let mut o = ScaleOptions::default();
        o.param0 = 1_000_000.0;
        o.param1 = 9_551_615.0;
        assert_eq!(o.luma_kernel(), Kernel::Bicubic { b: 0.0, c: 0.6 });
        assert_eq!(o.chroma_kernel(), Kernel::Bicubic { b: 0.0, c: 0.6 });

        o.param0 = -1_000_000.0;
        o.param1 = f64::INFINITY;
        assert_eq!(o.luma_kernel(), Kernel::Bicubic { b: 0.0, c: 0.6 });
    }

    /// The fallback in the test above must not have swallowed legitimate,
    /// in-range tuning: a caller experimenting within the documented `[-4,
    /// 4]` headroom still gets exactly the value they asked for.
    #[test]
    fn an_in_range_bicubic_parameter_is_honoured() {
        let mut o = ScaleOptions::default();
        o.param0 = 1.0;
        o.param1 = -2.5;
        assert_eq!(o.luma_kernel(), Kernel::Bicubic { b: 1.0, c: -2.5 });
    }

    #[test]
    fn bicublin_normalises_to_two_kernels() {
        let mut o = ScaleOptions::default();
        o.flags = SwsFlags::BICUBLIN;
        assert!(matches!(o.luma_kernel(), Kernel::Bicubic { .. }));
        assert_eq!(o.chroma_kernel(), Kernel::Bilinear);
    }

    #[test]
    fn options_parse_from_a_key_value_string() {
        let mut o = ScaleOptions::default();
        o.set_from_string("scaler=lanczos:param0=4:threads=3", "=", ":")
            .expect("parses");
        assert_eq!(o.scaler, ScalerKind::Lanczos);
        assert_eq!(o.luma_kernel(), Kernel::Lanczos { a: 4.0 });
        assert_eq!(o.threads, 3);
    }

    #[test]
    fn intent_and_lut_grid_parse_through_the_public_option_surface() {
        let mut o = ScaleOptions::default();
        o.set_from_string("intent=perceptual:lut3d_size=17", "=", ":")
            .expect("parses");
        assert_eq!(o.intent, RenderingIntent::Perceptual);
        assert_eq!(o.lut3d_size, 17);
    }

    #[test]
    fn legacy_flag_names_still_select_an_algorithm() {
        let mut o = ScaleOptions::default();
        o.set_from_string("sws_flags=lanczos", "=", ":")
            .expect("parses");
        assert!(matches!(o.luma_kernel(), Kernel::Lanczos { .. }));
    }
}
