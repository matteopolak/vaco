//! Plan 11 §6.4's worked example, transcribed.
//!
//! The `SwrContext` option table exercises aliases, units, ranges, bools,
//! floats, enums, flags, `sample_fmt` and `chlayout` in one place. It is here
//! as an executable check that the declaration in the plan compiles and behaves
//! as the plan says — the two substitutions are `SampleFormat`/`ChannelLayout`,
//! whose real impls live in layer-1 crates that do not exist yet.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::float_cmp,
    clippy::indexing_slicing,
    clippy::approx_constant
)]

mod support;

use support::{TestChLayout as ChannelLayout, TestSampleFmt as SampleFormat};
use vaco_opts::{
    OptBase, OptEnum, OptFlags, Options, OptionsExt, SerializeFlags, opt_flags, schema_of,
};

opt_flags! {
    /// force resampling even when the rates match
    #[unit = "swr_flags"]
    pub struct SwrFlags: u64 {
        /// force resampling even when the rates match
        const RES = 1 << 0 => "res";
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, OptEnum)]
#[opt_enum(unit = "dither_method", base = "int")]
pub enum DitherMethod {
    #[opt_const(name = "none", help = "no dithering")]
    #[default]
    None,
    #[opt_const(name = "rectangular", help = "rectangular dither")]
    Rectangular,
    #[opt_const(name = "triangular", help = "triangular dither")]
    Triangular,
    #[opt_const(name = "triangular_hp", help = "triangular dither with highpass")]
    TriangularHp,
    #[opt_const(name = "lipshitz", help = "Lipshitz noise shaping")]
    NsLipshitz,
    #[opt_const(name = "shibata", help = "Shibata noise shaping")]
    NsShibata,
}

#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "DitherOptions", help = "dithering")]
pub struct DitherOptions {
    #[opt(
        name = "dither_scale",
        help = "dither scale",
        default = 1.0,
        flags(audio, param)
    )]
    pub scale: f64,
}

#[derive(Debug, Clone, PartialEq, Options)]
#[options(name = "SwrContext", help = "audio resampling and rematrixing")]
pub struct ResampleOptions {
    #[opt(name = "isr", alias = "in_sample_rate", help = "input sample rate",
          default = 0, range = 0..=i32::MAX, flags(audio, param))]
    pub in_sample_rate: i32,

    #[opt(name = "osr", alias = "out_sample_rate", help = "output sample rate",
          default = 0, range = 0..=i32::MAX, flags(audio, param))]
    pub out_sample_rate: i32,

    #[opt(name = "isf", alias = "in_sample_fmt", help = "input sample format",
          default = SampleFormat::None, default_repr = "none", flags(audio, param))]
    pub in_sample_fmt: SampleFormat,

    #[opt(name = "ichl", alias = "in_chlayout", help = "input channel layout",
          default = ChannelLayout::Unspec, default_repr = "unspec", flags(audio, param))]
    pub in_chlayout: ChannelLayout,

    #[opt(name = "clev", alias = "center_mix_level", help = "center mix level",
          default = 0.707_106_78_f32, range = -32.0..=32.0, flags(audio, param))]
    pub center_mix_level: f32,

    #[opt(name = "flags", alias = "swr_flags", help = "engine flags",
          unit = "swr_flags", default = SwrFlags::empty(), default_repr = "0",
          flags(audio, param))]
    pub flags: SwrFlags,

    #[opt(name = "dither_method", help = "dither method",
          unit = "dither_method", default = DitherMethod::None, default_repr = "none",
          flags(audio, param))]
    pub dither_method: DitherMethod,

    #[opt(name = "phase_shift", help = "resampler phase shift",
          default = 10, range = 0..=24, flags(audio, param))]
    pub phase_shift: i32,

    #[opt(
        name = "linear_interp",
        help = "interpolate between filter phases",
        default = true,
        flags(audio, param)
    )]
    pub linear_interp: bool,

    #[opt(name = "cutoff", alias = "resample_cutoff", help = "cutoff frequency ratio",
          default = 0.0, range = 0.0..=1.0, flags(audio, param))]
    pub cutoff: f64,

    #[opt(name = "first_pts", help = "assumed first PTS, in samples",
          default = None, flags(audio, param))]
    pub first_pts: Option<i64>,

    /// Array-valued: an explicit channel remap, "0|1|2|3".
    #[opt(
        name = "channel_map",
        help = "explicit input->output channel map",
        array(sep = '|', max_len = 64),
        flags(audio, param)
    )]
    pub channel_map: Vec<i32>,

    /// A child object: its options are reachable by name from this one.
    #[opt(child)]
    pub dither: DitherOptions,

    /// Not an option; skipped entirely.
    #[opt(skip)]
    pub cached_matrix: Option<Vec<f32>>,
}

#[test]
fn the_plan_example_behaves_as_documented() {
    let mut o = ResampleOptions::default();
    assert_eq!(o.phase_shift, 10);
    assert!(o.linear_interp);
    assert_eq!(o.center_mix_level, 0.707_106_78_f32);
    assert_eq!(o.first_pts, None);
    assert!(o.channel_map.is_empty());

    o.set_from_string(
        "isr=48000:out_sample_rate=44100:dither_method=shibata:flags=+res:channel_map=0|1",
        "=",
        ":",
    )
    .unwrap();
    assert_eq!(o.in_sample_rate, 48000);
    assert_eq!(o.out_sample_rate, 44100);
    assert_eq!(o.dither_method, DitherMethod::NsShibata);
    assert_eq!(o.flags, SwrFlags::RES);
    assert_eq!(o.channel_map, vec![0, 1]);

    // Ranges are enforced against the typed value.
    assert!(o.set_str("phase_shift", "25").is_err());
    assert_eq!(o.phase_shift, 10);
    assert!(o.set_str("clev", "-33").is_err());

    // The child object's options resolve through the parent.
    o.set_str("dither_scale", "0.5").unwrap();
    assert_eq!(o.dither.scale, 0.5);

    let text = o.serialize(SerializeFlags {
        skip_defaults: true,
        ..SerializeFlags::default()
    });
    let mut p = ResampleOptions::default();
    p.set_from_string(&text, "=", ":").unwrap();
    assert_eq!(p, o);
}

#[test]
fn the_generated_schema_matches_the_plan() {
    let s = schema_of::<ResampleOptions>();
    assert_eq!(s.class_name, "SwrContext");
    assert_eq!(s.options.len(), 12);

    let isr = s.find("isr").unwrap();
    assert_eq!(isr.aliases, ["in_sample_rate"]);
    assert_eq!(isr.kind.base, OptBase::Int);
    assert!(isr.kind.array.is_none());
    assert!(isr.flags.contains(OptFlags::AUDIO));
    assert!(isr.flags.contains(OptFlags::ENCODING));
    assert!(isr.flags.contains(OptFlags::DECODING));
    assert_eq!(isr.default_repr, "0");
    assert_eq!(isr.range.unwrap().min, 0.0);
    assert_eq!(isr.id.0, 0);

    let dm = s.find("dither_method").unwrap();
    assert_eq!(dm.unit, Some("dither_method"));
    assert_eq!(dm.consts.len(), 6);
    assert_eq!(dm.default_repr, "none");
    assert_eq!(dm.id.0, 6);

    let cm = s.find("channel_map").unwrap();
    assert_eq!(cm.kind.base, OptBase::Int);
    assert_eq!(cm.kind.array.unwrap().sep, '|');
    assert_eq!(cm.kind.array.unwrap().max_len, 64);
    assert_eq!(cm.id.0, 11);

    // `#[opt(child)]` contributes a child schema, not an option.
    assert!(s.find("dither").is_none());
    assert_eq!(s.children.len(), 1);
    assert_eq!(s.children[0].class_name, "DitherOptions");
    // `#[opt(skip)]` contributes nothing at all.
    assert!(s.find("cached_matrix").is_none());
}

#[test]
fn units_are_shared_across_options() {
    // The mechanism the plan describes: constants belong to the unit, and any
    // option naming that unit sees them.
    let s = schema_of::<ResampleOptions>();
    let names: Vec<&str> = s.consts_for_unit("dither_method").map(|c| c.name).collect();
    assert_eq!(
        names,
        [
            "none",
            "rectangular",
            "triangular",
            "triangular_hp",
            "lipshitz",
            "shibata"
        ]
    );
    assert_eq!(s.consts_for_unit("swr_flags").count(), 1);
}
