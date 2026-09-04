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
        help = "dither method: none, rectangular, triangular, triangular_hp, \
                or one of the seven noise-shaping curves (lipshitz, f_weighted, \
                modified_e_weighted, improved_e_weighted, shibata, low_shibata, \
                high_shibata) — see vaco-resample's docs for what these are",
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
        // Delegate to `vaco-resample`'s own parser rather than re-listing its
        // option names here: a hardcoded copy of the mapping would silently
        // stop matching once `vaco-resample` changed its own curve set.
        match DitherMethod::from_name(self.dither_method.as_str()) {
            Ok(m) => m,
            Err(_) => DitherMethod::None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Opts,
    resampler: Option<Resampler>,
    input: Option<AudioSpec>,
    output: Option<AudioSpec>,
    /// Where the *next* emitted frame's PTS starts, in output-sample units —
    /// the running end of whatever was last emitted, by [`filter_frame`] or
    /// by [`flush`] itself (called repeatedly until it answers
    /// [`FrameOut::None`], and every one of those calls after the first has
    /// no input frame to derive a timestamp from). `NONE` until the first
    /// frame has actually gone through.
    ///
    /// [`filter_frame`]: FrameFilter::filter_frame
    /// [`flush`]: FrameFilter::flush
    next_pts: vaco_core::Timestamp,
}

impl Filter {
    fn new(opts: Opts) -> Self {
        Self {
            opts,
            resampler: None,
            input: None,
            output: None,
            next_pts: vaco_core::Timestamp::NONE,
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

    /// Record where the *next* emitted frame's PTS should start, given that
    /// this one started at `pts` and carried `written` output samples.
    fn advance_next_pts(&mut self, pts: vaco_core::Timestamp, written: usize) {
        if let Some(ticks) = pts.ticks() {
            self.next_pts =
                vaco_core::Timestamp::new(ticks.saturating_add_unsigned(written as u64));
        }
    }
}

/// Shrink an audio frame allocated for an *estimated* sample count down to
/// the number a resampler actually wrote.
///
/// [`vaco_resample::Resampler::out_samples`] is documented as an upper
/// bound, not an exact count — the exact-rational engine's own per-chunk
/// output legitimately varies by a sample or two either way, which is
/// exactly why a caller has to ask before allocating. `filter_frame`/`flush`
/// allocate for that upper bound, so a plane whose declared `samples` and
/// actual buffer length both equal `want` still holds only `written` real
/// samples with whatever the pool's previous tenant left in the rest.
///
/// Nothing downstream is told to stop at `written`: `Frame::samples` is not
/// consulted by every reader, and `Plane::data` carries no distinct
/// logical-vs-allocated length the way a video plane's `stride` does for
/// row width. `vaco-codec-pcm`'s encoder is the concrete case this closes —
/// it encodes `plane.data.as_slice()` whole — but nothing guarantees every
/// other consumer respects `samples` either, so the frame itself has to be
/// the true size before it leaves this filter, not merely labelled with one.
/// Measured effect before this existed: `out_samples()` overshot `written`
/// by 590 samples across a 3-second clip (36 chunked `filter_frame` calls
/// plus one `flush`), and every one of those samples was stale pool
/// content encoded as if it were real audio — a garbage sample scattered
/// roughly every 4096 real ones, which is why the corruption measured as a
/// small, *uniform* pitch shift (438 Hz instead of 440) rather than an
/// isolated glitch: densely spread structured error, not an edge artefact.
fn shrink_to_written(ctx: &mut FilterContext<'_>, frame: Frame, written: usize) -> Result<Frame> {
    let vaco_frame::FrameData::Audio {
        format,
        sample_rate,
        samples,
        layout,
        ..
    } = &frame.data
    else {
        return Err(Error::InvalidData("aresample produced a non-audio frame"));
    };
    if written as u64 >= u64::from(*samples) {
        // Already exact (or, defensively, the estimate undershot — nothing
        // to trim either way).
        return Ok(frame);
    }
    let (format, sample_rate, layout) = (*format, *sample_rate, layout.clone());
    let pool = ctx.pool().clone();
    let mut shrunk = pool.acquire_audio(
        format,
        layout,
        u32::try_from(written).unwrap_or(0),
        sample_rate,
    )?;
    {
        let vaco_frame::FrameData::Audio {
            planes: src_planes, ..
        } = &frame.data
        else {
            return Err(Error::InvalidData("aresample produced a non-audio frame"));
        };
        let vaco_frame::FrameData::Audio {
            planes: dst_planes, ..
        } = &mut shrunk.data
        else {
            return Err(Error::InvalidData(
                "a freshly allocated audio frame is not audio",
            ));
        };
        for (src, dst) in src_planes.iter().zip(dst_planes.iter_mut()) {
            let dst_buf = dst.data.make_mut();
            let n = dst_buf.len().min(src.data.as_slice().len());
            if let (Some(d), Some(s)) = (dst_buf.get_mut(..n), src.data.as_slice().get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    shrunk.pts = frame.pts;
    shrunk.time_base = frame.time_base;
    shrunk.flags = frame.flags;
    Ok(shrunk)
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
                // A packed frame always has exactly one plane regardless of
                // channel count — `refs.len()` here is always `1` and was
                // being handed to `AudioMut::packed` as the channel count,
                // which happened to be right only for mono (the one case
                // this filter's own channel-layout request had ever been
                // wired to before `-ac` reached it: see `target_formats`).
                // A packed multi-channel output — `-ac 2` onto a mono
                // source, say — built a buffer this resampler considered
                // `output buffer does not match the spec` and refused
                // outright.
                let channels = output_spec.layout.channels;
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
        // `want` is an upper bound, not the count the resampler actually
        // produced this call (see `shrink_to_written`'s own doc) — without
        // this, the tail of every frame past `written` is whatever the pool
        // buffer previously held, encoded as if it were real audio.
        let mut out_frame = shrink_to_written(ctx, out_frame, written)?;
        out_frame.pts = resampler_pts(resampler, &input);
        out_frame.time_base =
            vaco_core::Rational::new(1, i32::try_from(output_spec.sample_rate).unwrap_or(1));
        out_frame.set_duration_ticks(i64::try_from(written).unwrap_or(0));
        self.advance_next_pts(out_frame.pts, written);
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

        // The output link already carries what negotiation decided this
        // instance should produce, and that is the one correct source of
        // truth here — not `self.opts`. `target_formats` fed negotiation an
        // `Exact` constraint for whichever of format/rate/layout this
        // instance's own options requested and a *tie* to the input for the
        // rest (see that function's doc), so the negotiated link already
        // equals "what `self.opts` asked for, or the input's own value" for
        // an explicitly-configured `-af aresample=...` instance.
        //
        // Recomputing from `self.opts` instead — as this used to do — is
        // only ever consistent with that for an explicit instance, because
        // its own options are the only input negotiation had. It is wrong
        // for an **auto-inserted** instance: `vaco-filter-graph`'s converter
        // factory builds that one from `-aresample_swr_opts` alone (never
        // the target format it computed), so `self.opts.sample_rate` is `0`
        // regardless of what the graph actually needs — the auto-inserted
        // aresample fixing `-ar 44100` against a 48000 Hz source produced
        // 48000 Hz output, silently, because it fell back to "same as
        // input" exactly as if nothing had asked for a change at all.
        let Some(LinkFormat::Audio {
            format: out_fmt,
            sample_rate: out_rate,
            layout: out_layout,
            ..
        }) = ctx.output_link(0).cloned()
        else {
            return Err(Error::Unsupported(
                "aresample needs a configured audio output",
            ));
        };
        self.output = Some(AudioSpec {
            sample_rate: out_rate,
            format: out_fmt,
            layout: out_layout,
        });
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
                // See the identical fix in `filter_frame`: a packed frame's
                // plane count is always one, not the channel count.
                let channels = output_spec.layout.channels;
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
        // Same reason as `filter_frame`: `want` is an upper bound on this
        // call's output, not what it actually produced.
        let mut out_frame = shrink_to_written(ctx, out_frame, written)?;
        // `flush` is called repeatedly with no input frame to derive a PTS
        // from — unlike `filter_frame`, which has `resampler_pts` for exactly
        // that — so it continues from wherever the last emitted frame left
        // off. Before this, a flushed frame carried no PTS at all, which a
        // container that requires one (this crate's own regression test
        // reproduces the reference's own message) rejected outright even
        // though the bytes it carried were correct.
        out_frame.pts = if self.next_pts.ticks().is_some() {
            self.next_pts
        } else {
            // No frame ever went through `filter_frame` before end of
            // stream (a zero-sample input, or a graph that flushes
            // immediately) — there is no "last emitted frame" to continue
            // from, so start the flushed frame's own clock at zero rather
            // than propagate `NONE` into a container that rejects it.
            vaco_core::Timestamp::new(0)
        };
        out_frame.time_base =
            vaco_core::Rational::new(1, i32::try_from(output_spec.sample_rate).unwrap_or(1));
        out_frame.set_duration_ticks(i64::try_from(written).unwrap_or(0));
        self.advance_next_pts(out_frame.pts, written);
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
    let mut ties = Vec::new();

    let requested_format = (!opts.out_sample_fmt.is_empty())
        .then(|| SampleFmt::from_name(&opts.out_sample_fmt).ok())
        .flatten();
    if let Some(fmt) = requested_format {
        out.sample_formats = Some(Constraint::Exact(fmt));
    } else {
        // No `out_sample_fmt`: this instance changes rate/layout only, so its
        // sample format is whatever arrives — a *tie*, not `FormatSet::default()`'s
        // `Any`. `pan`'s `target_formats` documents the same distinction:
        // an unconstrained, untied output pad is a requirement negotiation
        // still has to solve, and with nothing downstream to solve it (a sink
        // with no opinion, same as this one when the user only asked for a
        // sample-rate change) that is exactly "format negotiation left a
        // property unconstrained" — the graph never crashed on the *value*,
        // it crashed because nothing ever supplied one.
        ties.push(vaco_filter_core::negotiate::Tie {
            property: vaco_filter_core::negotiate::Property::SampleFormat,
            pads: vec![
                (vaco_filter_core::link::Direction::Input, 0),
                (vaco_filter_core::link::Direction::Output, 0),
            ],
        });
    }

    if opts.sample_rate > 0 {
        out.sample_rates = Some(Constraint::Exact(opts.sample_rate as u32));
    } else {
        ties.push(vaco_filter_core::negotiate::Tie {
            property: vaco_filter_core::negotiate::Property::SampleRate,
            pads: vec![
                (vaco_filter_core::link::Direction::Input, 0),
                (vaco_filter_core::link::Direction::Output, 0),
            ],
        });
    }

    let requested_layout = (!opts.out_chlayout.is_empty())
        .then(|| ChannelLayout::from_name(&opts.out_chlayout))
        .flatten();
    if let Some(layout) = requested_layout {
        out.channel_layouts = Some(Constraint::Exact(layout));
    } else {
        ties.push(vaco_filter_core::negotiate::Tie {
            property: vaco_filter_core::negotiate::Property::ChannelLayout,
            pads: vec![
                (vaco_filter_core::link::Direction::Input, 0),
                (vaco_filter_core::link::Direction::Output, 0),
            ],
        });
    }

    NodeFormats {
        inputs: vec![FormatSet::default()],
        outputs: vec![out],
        ties,
        label: label.to_owned(),
    }
}

use vaco_frame::Frame;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code; the division computing the expected exact-ratio sample \
              count is deliberately integer arithmetic, not a precision accident"
)]
mod tests {
    use vaco_core::{MediaType, Timestamp};
    use vaco_filter_core::Graph;
    use vaco_filter_core::mock::{any_audio_sink, audio_frame, audio_link, audio_source_formats};
    use vaco_frame::FrameData;

    use vaco_filter_graph::registry::Instantiate;

    use super::create;

    /// A 48000 Hz source through `aresample=sample_rate=44100`, fed in
    /// chunks the way a real decoder does (irregular sizes, not neat
    /// multiples of anything), must report a total sample count matching
    /// the exact `44100/48000` ratio — not the per-call *estimate*
    /// [`vaco_resample::Resampler::out_samples`] hands back before it knows
    /// what a given chunk will actually produce.
    ///
    /// This is the regression for a real defect: `filter_frame`/`flush`
    /// used to allocate each output frame for that estimate and hand it
    /// downstream un-shrunk, so a consumer reading the frame's own buffer
    /// (`vaco-codec-pcm`'s encoder among them) encoded whatever the pool's
    /// previous tenant had left in the slack past the real output — 590
    /// stale samples over a 3-second clip, measured as a uniform ~0.45%
    /// pitch shift in the muxed audio because the garbage was spread every
    /// ~4096 samples rather than confined to one place.
    #[test]
    fn chunked_downsampling_reports_exactly_the_resamplers_real_total() {
        let in_rate = 48_000u32;
        let out_rate = 44_100u32;
        let total_in: u32 = 144_000;

        let req = Instantiate {
            name: "aresample",
            instance: "aresample",
            args: Some("sample_rate=44100"),
            arguments: &[],
        };
        let inst = create(&req).unwrap();

        let mut graph = Graph::new();
        let src = graph.add_source("in", MediaType::Audio, audio_source_formats("in", in_rate));
        let node = graph.add(inst.desc, inst.formats, inst.filter);
        let sink = graph.add_sink("out", MediaType::Audio, any_audio_sink("out"));
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        graph.set_source_format(src, audio_link(in_rate)).unwrap();
        graph.configure().unwrap();

        // Irregular chunk sizes, deliberately not evenly dividing `total_in`
        // — a real WAV decoder's `TARGET_PACKET`-sized packets do not either
        // (the file's last packet is always a remainder).
        let chunk = 4096u32;
        let mut sent = 0u32;
        let mut total_reported = 0u64;
        let mut pts = 0i64;
        while sent < total_in {
            let n = chunk.min(total_in - sent);
            graph.send(src, audio_frame(in_rate, n, pts)).unwrap();
            pts += i64::from(n);
            sent += n;
            graph.run().unwrap();
            while let Ok(f) = graph.recv(sink) {
                let FrameData::Audio {
                    samples,
                    planes,
                    format,
                    layout,
                    ..
                } = &f.data
                else {
                    panic!("expected an audio frame");
                };
                // The other half of the regression: each plane's actual byte
                // length must equal exactly what `samples` declares, not
                // merely have a total that matches by coincidence of the two
                // bugs cancelling out. A packed plane carries every channel
                // interleaved; a planar one carries one channel per plane.
                let bytes_per_sample = format.bytes_per_sample() as u64;
                let per_plane_channels = if format.is_planar() {
                    1
                } else {
                    layout.channels.max(1)
                };
                let expected_bytes =
                    u64::from(*samples) * u64::from(per_plane_channels) * bytes_per_sample;
                for plane in planes {
                    assert_eq!(plane.data.as_slice().len() as u64, expected_bytes);
                }
                total_reported += u64::from(*samples);
            }
        }
        graph
            .close_source(src, Timestamp::new(i64::from(total_in)))
            .unwrap();
        graph.run().unwrap();
        while let Ok(f) = graph.recv(sink) {
            let FrameData::Audio { samples, .. } = &f.data else {
                panic!("expected an audio frame");
            };
            total_reported += u64::from(*samples);
        }

        let expected = u64::from(total_in) * u64::from(out_rate) / u64::from(in_rate);
        assert_eq!(total_reported, expected);
    }
}
