//! A reference option object exercising every base, plus stand-ins for the
//! three types whose `OptValue` impls live in layer-1 crates.
//!
//! Implementing `PixelFmt`/`SampleFmt`/`ChLayout` here from outside the crate
//! is not just convenience: it is the test that the F6 dependency inversion
//! actually works, i.e. that a foreign crate can contribute an option base
//! without `vaco-opts` knowing anything about it.

#![allow(dead_code, unreachable_pub)]

use vaco_core::{Duration, Rational};
use vaco_opts::{
    Binary, Dict, OptBase, OptEnum, OptError, OptValue, OptValueKind, Options, ParseCtx, Rgba,
    SerCtx, VideoRate, impl_opt_value_common, opt_flags,
};

// --------------------------------------------------- layer-1 stand-in types

macro_rules! named_enum_value {
    ($t:ident, $base:expr, $( $v:ident => $s:literal ),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
        pub enum $t { #[default] $( $v ),+ }

        impl OptValueKind for $t {
            const BASE: OptBase = $base;
        }

        impl OptValue for $t {
            fn parse_into(&mut self, s: &str, ctx: &ParseCtx<'_>) -> Result<(), OptError> {
                *self = match s.trim() {
                    $( $s => $t::$v, )+
                    _ => return Err(OptError::InvalidValue {
                        name: ctx.name.to_owned(),
                        value: s.to_owned(),
                    }),
                };
                Ok(())
            }
            fn serialize(&self, out: &mut String, _ctx: &SerCtx<'_>) {
                out.push_str(match self { $( $t::$v => $s ),+ });
            }
            impl_opt_value_common!($t);
        }
    };
}

named_enum_value!(TestPixFmt, OptBase::PixelFmt, None => "none", Yuv420p => "yuv420p", Rgb24 => "rgb24");
named_enum_value!(TestSampleFmt, OptBase::SampleFmt, None => "none", S16 => "s16", Fltp => "fltp");
named_enum_value!(TestChLayout, OptBase::ChLayout, Unspec => "unspec", Mono => "mono", Stereo => "stereo");

// ------------------------------------------------------------ flags & enum

opt_flags! {
    /// engine flags
    #[unit = "tflags"]
    pub struct TFlags: u64 {
        /// low delay
        const LOW_DELAY = 1 << 0 => "low_delay";
        /// bit-exact output
        const BITEXACT = 1 << 1 => "bitexact";
        /// unaligned access permitted
        const UNALIGNED = 1 << 2 => "unaligned";
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "tmethod", base = "int")]
pub enum TMethod {
    #[opt_const(name = "none", help = "no dithering")]
    #[default]
    None,
    #[opt_const(name = "rectangular", help = "rectangular dither")]
    Rectangular,
    #[opt_const(name = "triangular", help = "triangular dither")]
    Triangular,
    #[opt_const(name = "shibata", help = "Shibata noise shaping")]
    Shibata = 17,
}

// ---------------------------------------------------------------- the object

#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "ChildOpts", help = "a nested object")]
pub struct ChildOpts {
    #[opt(name = "child_gain", help = "gain", default = 1.0, range = 0.0..=10.0, flags(audio, runtime))]
    pub gain: f64,
    #[opt(name = "child_label", help = "label", flags(audio))]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "AllKinds", help = "every option base in one place")]
pub struct AllKinds {
    #[opt(name = "flags", alias = "tflags", help = "engine flags", unit = "tflags",
          default = TFlags::empty(), default_repr = "0", flags(video, param))]
    pub flags: TFlags,

    #[opt(name = "i", help = "an int", default = 0, range = -1000..=1000, flags(video, runtime))]
    pub i: i32,

    #[opt(name = "i64", help = "an int64", default = 0, range = i64::MIN..=i64::MAX, flags(video))]
    pub i64v: i64,

    #[opt(name = "u", help = "a uint", default = 0, range = 0..=100, flags(video))]
    pub u: u32,

    #[opt(name = "u64", help = "a uint64", default = 0, flags(video))]
    pub u64v: u64,

    #[opt(name = "d", help = "a double", default = 0.5, range = -1.0..=1.0, flags(video))]
    pub d: f64,

    #[opt(name = "f", help = "a float", default = 0.25, flags(video))]
    pub f: f32,

    #[opt(name = "b", help = "a bool", default = false, flags(video))]
    pub b: bool,

    #[opt(name = "tri", help = "a tri-state bool", default = None, flags(video))]
    pub tri: Option<bool>,

    #[opt(name = "s", help = "a string", flags(video))]
    pub s: String,

    #[opt(name = "r", help = "a rational", flags(video))]
    pub r: Rational,

    #[opt(name = "bin", help = "opaque bytes", flags(video))]
    pub bin: Binary,

    #[opt(name = "dict", help = "a nested dictionary", flags(video))]
    pub dict: Dict,

    #[opt(name = "size", help = "an image size", flags(video))]
    pub size: (u32, u32),

    #[opt(name = "pixfmt", help = "a pixel format", flags(video))]
    pub pixfmt: TestPixFmt,

    #[opt(name = "samplefmt", help = "a sample format", flags(audio))]
    pub samplefmt: TestSampleFmt,

    #[opt(name = "chlayout", help = "a channel layout", flags(audio))]
    pub chlayout: TestChLayout,

    #[opt(name = "rate", help = "a frame rate", flags(video))]
    pub rate: VideoRate,

    #[opt(name = "dur", help = "a duration", flags(video))]
    pub dur: Duration,

    #[opt(name = "colour", help = "a colour", flags(video))]
    pub colour: Rgba,

    #[opt(name = "method", help = "a named method", unit = "tmethod",
          default = TMethod::None, default_repr = "none", flags(video))]
    pub method: TMethod,

    #[opt(
        name = "arr",
        help = "an int array",
        array(sep = '|', max_len = 8),
        flags(video)
    )]
    pub arr: Vec<i32>,

    #[opt(name = "sarr", help = "a string array", array(sep = ','), flags(video))]
    pub sarr: Vec<String>,

    #[opt(name = "opt_i", help = "an optional int64", default = None, flags(video))]
    pub opt_i: Option<i64>,

    #[opt(child)]
    pub child: ChildOpts,

    #[opt(skip)]
    pub cache: Option<u8>,
}
