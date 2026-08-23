//! `aresample` — resample, reformat and rematrix audio.
//!
//! Wraps `vaco-resample`'s [`Resampler`], which is the actual conversion
//! engine (rate, format and channel-layout conversion, dither). This module's
//! job is the plumbing: turn the filter's options into a target
//! [`FormatSet`](vaco_filter_core::negotiate::FormatSet) so negotiation picks
//! the requested output configuration, then drive the resampler from
//! `filter_frame`/`flush`.
//!
//! # Measured against the reference
//!
//! `ffmpeg -h filter=aresample` (`LC_ALL=C`, ffmpeg 8.1) documents one
//! filter-local option (`sample_rate`) plus the entire `SWResampler` class —
//! about 40 options. This implementation covers the ones that change output
//! samples on the common path: `sample_rate`/`osr`/`out_sample_rate`,
//! `out_sample_fmt`/`osf`, `out_chlayout`/`ochl`, the three mix levels,
//! `rematrix_volume`, `filter_size`, `phase_shift`, `linear_interp`, `cutoff`
//! and `dither_method`. Not implemented: `first_pts` and the whole
//! timestamp-compensation group (`async`, `min_comp`, `min_hard_comp`,
//! `comp_duration`, `max_soft_comp`), `matrix_encoding`, and the `soxr`
//! engine (accepted upstream as an alias to the native engine — see
//! `vaco-resample`'s own docs — so requesting it here is simply ignored
//! rather than rejected).

use vaco_chlayout::ChannelLayout;
use vaco_core::{Error, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_opts::OptionsExt as _;
use vaco_resample::buf::AudioSpec;
use vaco_resample::opts::ResampleOptions;
use vaco_resample::{Resampler, dither::DitherMethod};
use vaco_sampfmt::SampleFmt;

use vaco_filter_graph::registry::Instance;
use vaco_filter_graph::registry::Instantiate;

const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "aresample",
    description: "resample audio data",
    inputs: AUDIO_PAD,
    outputs: AUDIO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "aresample", help = "resample audio data")]
pub(crate) struct Opts {
    #[opt(
        name = "sample_rate",
        alias = "osr,out_sample_rate",
        help = "output sample rate, 0 = unchanged",
        default = 0,
        range = 0..=i32::MAX,
        flags(audio, filtering)
    )]
    pub sample_rate: i32,

    /// Kept as a raw name rather than `SampleFmt` directly: neither
    /// `vaco-sampfmt` nor `vaco-chlayout` implements `vaco_opts::OptValue`
    /// yet (the same gap `vaco-resample`'s own `ResampleOptions` documents),
    /// so the endpoint type is parsed by hand in [`create`].
    #[opt(
        name = "out_sample_fmt",
        alias = "osf",
        help = "output sample format, empty = unchanged",
        default = String::new(),
        flags(audio, filtering)
    )]
    pub out_sample_fmt: String,

    #[opt(
        name = "out_chlayout",
        alias = "ochl",
        help = "output channel layout, empty = unchanged",
        default = String::new(),
        flags(audio, filtering)
    )]
    pub out_chlayout: String,

    #[opt(
        name = "center_mix_level",
        alias = "clev",
        help = "center mix level",
        default = 0.707_106_78_f64,
        range = -32.0..=32.0,
        flags(audio, filtering)
    )]
    pub center_mix_level: f64,

    #[opt(
        name = "surround_mix_level",
        alias = "slev",
        help = "surround mix level",
        default = 0.707_106_78_f64,
        range = -32.0..=32.0,
        flags(audio, filtering)
    )]
    pub surround_mix_level: f64,

    #[opt(
        name = "lfe_mix_level",
        help = "LFE mix level",
        default = 0.0,
        range = -32.0..=32.0,
        flags(audio, filtering)
    )]
    pub lfe_mix_level: f64,

    #[opt(
        name = "rematrix_volume",
        alias = "rmvol",
        help = "rematrix volume",
        default = 1.0,
        range = -1000.0..=1000.0,
        flags(audio, filtering)
    )]
    pub rematrix_volume: f64,

    #[opt(
        name = "filter_size",
        help = "resampling filter size",
        default = 32,
        range = 0..=1024,
        flags(audio, filtering)
    )]
    pub filter_size: i32,

    #[opt(
        name = "phase_shift",
        help = "resampling phase shift",
        default = 10,
        range = 0..=24,
        flags(audio, filtering)
    )]
    pub phase_shift: i32,

    #[opt(
        name = "linear_interp",
        help = "enable linear interpolation",
        default = false,
        flags(audio, filtering)
    )]
    pub linear_interp: bool,

    #[opt(
        name = "cutoff",
        help = "cutoff frequency ratio",
        default = 0.0,
        range = 0.0..=1.0,
        flags(audio, filtering)
    )]
    pub cutoff: f64,

    #[opt(
        name = "dither_method",
        help = "dither method: none, rectangular or triangular",
        default = "none".to_owned(),
        flags(audio, filtering)
    )]
    pub dither_method: String,
}

impl Opts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }

    fn dither(&self) -> DitherMethod {
        match self.dither_method.as_str() {
            "rectangular" => DitherMethod::Rectangular,
            // The noise-shaping curves (`lipshitz`, `shibata`, ...) are aliased
            // to triangular-highpass by `vaco-resample` itself; see its docs.
            "triangular"
            | "triangular_hp"
            | "lipshitz"
            | "shibata"
            | "low_shibata"
            | "high_shibata"
            | "f_weighted"
            | "modified_e_weighted"
            | "improved_e_weighted" => DitherMethod::TriangularHighpass,
            _ => DitherMethod::None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Opts,
    resampler: Option<Resampler>,
    input: Option<AudioSpec>,
    output: Option<AudioSpec>,
}

impl Filter {
    fn new(opts: Opts) -> Self {
        Self {
            opts,
            resampler: None,
            input: None,
            output: None,
        }
    }

    fn ensure_resampler(&mut self) -> Result<()> {
        if self.resampler.is_some() {
            return Ok(());
        }
        let (Some(input), Some(output)) = (self.input.clone(), self.output.clone()) else {
            return Err(Error::Unsupported(
                "aresample activated before its links were configured",
            ));
        };
        let ropts = ResampleOptions {
            center_mix_level: self.opts.center_mix_level as f32,
            surround_mix_level: self.opts.surround_mix_level as f32,
            lfe_mix_level: self.opts.lfe_mix_level as f32,
            rematrix_volume: self.opts.rematrix_volume as f32,
            filter_size: self.opts.filter_size,
            phase_shift: self.opts.phase_shift,
            linear_interp: self.opts.linear_interp,
            cutoff: self.opts.cutoff,
            dither_method: self.opts.dither(),
            ..ResampleOptions::default()
        };
        let mut budget = vaco_limits::Budget::new(vaco_limits::Limits::permissive());
        let resampler = Resampler::new(&input, &output, &ropts, &mut budget)?;
        self.resampler = Some(resampler);
        Ok(())
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        self.ensure_resampler()?;
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(FrameOut::None);
        };
        let Some(output_spec) = self.output.clone() else {
            return Ok(FrameOut::None);
        };
        let vaco_frame::FrameData::Audio {
            format,
            samples,
            layout,
            ..
        } = &input.data
        else {
            return Err(Error::InvalidData("aresample given a non-audio frame"));
        };
        let (fmt, samples, channels) = (*format, *samples, layout.channels.max(1));
        let mut src_planes: smallvec::SmallVec<[&[u8]; 8]> = smallvec::SmallVec::new();
        for i in 0..input.plane_count() {
            let Some(p) = input.plane(i) else { break };
            src_planes.push(p.as_slice());
        }
        let src = vaco_resample::buf::AudioRef::from_frame_planes(fmt, channels, &src_planes)
            .map_err(|_| Error::InvalidData("aresample given a malformed audio frame"))?;

        let want = resampler.out_samples(samples as usize).max(1);
        let pool = ctx.pool().clone();
        let mut out_frame = pool.acquire_audio(
            output_spec.format,
            output_spec.layout.clone(),
            u32::try_from(want).unwrap_or(u32::MAX),
            output_spec.sample_rate,
        )?;
        let written = {
            let mut planes = out_frame.planes_mut();
            let mut refs: Vec<&mut [u8]> = Vec::new();
            for p in &mut planes {
                if let Some(row) = p.row_mut(0) {
                    refs.push(row);
                }
            }
            let mut dst = if output_spec.format.is_planar() {
                vaco_resample::buf::AudioMut::planar(output_spec.format, &mut refs)
            } else {
                let channels = u32::try_from(refs.len()).unwrap_or(0);
                let Some(buf) = refs.into_iter().next() else {
                    return Err(Error::InvalidData("aresample output frame has no plane"));
                };
                vaco_resample::buf::AudioMut::packed(output_spec.format, channels, buf)
            }
            .map_err(|_| Error::InvalidData("could not build an aresample output buffer"))?;
            resampler.convert(Some(src), &mut dst)?
        };

        if written == 0 {
            return Ok(FrameOut::None);
        }
        out_frame.pts = resampler_pts(resampler, &input);
        out_frame.time_base =
            vaco_core::Rational::new(1, i32::try_from(output_spec.sample_rate).unwrap_or(1));
        out_frame.duration = vaco_core::Duration(i64::try_from(written).unwrap_or(0));
        Ok(FrameOut::One(out_frame))
    }

    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let Some(LinkFormat::Audio {
            format,
            sample_rate,
            layout,
            ..
        }) = ctx.input_link(0).cloned()
        else {
            return Err(Error::Unsupported(
                "aresample needs a configured audio input",
            ));
        };
        self.input = Some(AudioSpec {
            sample_rate,
            format,
            layout,
        });

        let out_fmt = if self.opts.out_sample_fmt.is_empty() {
            format
        } else {
            SampleFmt::from_name(&self.opts.out_sample_fmt).unwrap_or(format)
        };
        let out_rate = if self.opts.sample_rate > 0 {
            self.opts.sample_rate as u32
        } else {
            sample_rate
        };
        let out_layout = if self.opts.out_chlayout.is_empty() {
            ctx.input_link(0)
                .and_then(|l| match l {
                    LinkFormat::Audio { layout, .. } => Some(layout.clone()),
                    LinkFormat::Video { .. } => None,
                })
                .unwrap_or(ChannelLayout::STEREO)
        } else {
            ChannelLayout::from_name(&self.opts.out_chlayout).unwrap_or(ChannelLayout::STEREO)
        };
        self.output = Some(AudioSpec {
            sample_rate: out_rate,
            format: out_fmt,
            layout: out_layout.clone(),
        });

        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                format,
                sample_rate,
                layout,
                time_base,
            } = &mut out
            {
                *format = out_fmt;
                *sample_rate = out_rate;
                *layout = out_layout;
                *time_base = vaco_core::Rational::new(1, i32::try_from(out_rate).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        self.ensure_resampler()?;
        let Some(output_spec) = self.output.clone() else {
            return Ok(FrameOut::None);
        };
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(FrameOut::None);
        };
        let want = resampler.out_samples(0).max(1);
        let pool = ctx.pool().clone();
        let mut out_frame = pool.acquire_audio(
            output_spec.format,
            output_spec.layout.clone(),
            u32::try_from(want).unwrap_or(1),
            output_spec.sample_rate,
        )?;
        let written = {
            let mut planes = out_frame.planes_mut();
            let mut refs: Vec<&mut [u8]> = Vec::new();
            for p in &mut planes {
                if let Some(row) = p.row_mut(0) {
                    refs.push(row);
                }
            }
            let mut dst = if output_spec.format.is_planar() {
                vaco_resample::buf::AudioMut::planar(output_spec.format, &mut refs)
            } else {
                let channels = u32::try_from(refs.len()).unwrap_or(0);
                let Some(buf) = refs.into_iter().next() else {
                    return Ok(FrameOut::None);
                };
                vaco_resample::buf::AudioMut::packed(output_spec.format, channels, buf)
            }
            .map_err(|_| Error::InvalidData("could not build an aresample flush buffer"))?;
            resampler.convert(None, &mut dst)?
        };
        if written == 0 {
            return Ok(FrameOut::None);
        }
        out_frame.duration = vaco_core::Duration(i64::try_from(written).unwrap_or(0));
        Ok(FrameOut::One(out_frame))
    }

    fn flush_state(&mut self) {
        if let Some(r) = self.resampler.as_mut() {
            r.reset();
        }
    }
}

/// The output PTS for a frame that just went through `resampler`: the
/// resampler's own delay estimate rebased onto the input frame's own PTS in
/// output-sample units, or `NONE` if the input carried no timestamp.
fn resampler_pts(resampler: &Resampler, input: &vaco_frame::Frame) -> vaco_core::Timestamp {
    let Some(in_pts) = input.pts.ticks() else {
        return vaco_core::Timestamp::NONE;
    };
    let out_tb = vaco_core::Rational::new(
        1,
        i32::try_from(resampler.output_spec().sample_rate).unwrap_or(1),
    );
    let rescaled = vaco_core::Timestamp::new(in_pts)
        .rescale(input.time_base, out_tb, vaco_core::Rounding::Down)
        .ticks()
        .unwrap_or(in_pts);
    vaco_core::Timestamp::new(resampler.next_pts(rescaled))
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let formats = target_formats(&opts, req.instance);
    Ok(Instance {
        desc: DESC,
        formats,
        filter: Box::new(Simple::new(Filter::new(opts))),
    })
}

fn target_formats(opts: &Opts, label: &str) -> NodeFormats {
    let mut out = FormatSet::default();
    if !opts.out_sample_fmt.is_empty()
        && let Ok(fmt) = SampleFmt::from_name(&opts.out_sample_fmt)
    {
        out.sample_formats = Some(Constraint::Exact(fmt));
    }
    if opts.sample_rate > 0 {
        out.sample_rates = Some(Constraint::Exact(opts.sample_rate as u32));
    }
    if !opts.out_chlayout.is_empty()
        && let Some(layout) = ChannelLayout::from_name(&opts.out_chlayout)
    {
        out.channel_layouts = Some(Constraint::Exact(layout));
    }
    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: vec![out],
        ties: Vec::new(),
        label: label.to_owned(),
    }
}

use vaco_frame::Frame;
