//! `nullsrc`/`anullsrc` — generate frames with no meaningful content.
//!
//! `ffmpeg -h filter=nullsrc` documents `size`/`s` (default `"320x240"`),
//! `rate`/`r` (default `"25"`) and `duration`/`d` (default `-0.000001`,
//! meaning unlimited — the reference's own sentinel, reproduced exactly
//! rather than translated to `None`, per D9: it is an observable default
//! value on `-h filter=`). `anullsrc` documents `channel_layout`/`cl`
//! (default `"stereo"`), `sample_rate`/`r` (default 44100), `nb_samples`/`n`
//! (default 1024) and `duration`/`d` (same sentinel).
//!
//! Both are implemented completely: `vaco_core::parse::{image_size,
//! video_rate, duration}` already speak the reference's exact string
//! grammar (`"320x240"`, `"25"`/`"ntsc"`, `"-1"`/`"10"`), so this filter is
//! almost entirely option parsing plus a source loop.

use vaco_core::{Duration as VDuration, MediaType, Rational, Result, Timestamp};
use vaco_filter_core::adapt::{SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;
use vaco_pixfmt::PixFmt;

use vaco_filter_graph::registry::{Instance, Instantiate};

/// The reference's own "unlimited" sentinel for `duration`/`d`: a single tick
/// under zero. Reproduced rather than translated to `None` because it is
/// what `-h filter=nullsrc` actually prints as the default.
const UNLIMITED: VDuration = VDuration::from_micros(-1);

// -------------------------------------------------------------- nullsrc

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "nullsrc", help = "null video source")]
pub(crate) struct VideoOpts {
    #[opt(name = "size", alias = "s", help = "set video size", default = (320, 240), flags(filtering))]
    pub size: (u32, u32),
    #[opt(name = "rate", alias = "r", help = "set video rate", default = vaco_opts::VideoRate(Rational::new(25, 1)), flags(filtering))]
    pub rate: vaco_opts::VideoRate,
    #[opt(name = "duration", alias = "d", help = "set video duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
    #[opt(name = "sar", help = "set video sample aspect ratio", default = Rational::ONE, flags(filtering))]
    pub sar: Rational,
}

impl VideoOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct VideoSource {
    width: u32,
    height: u32,
    frame_rate: Rational,
    sar: Rational,
    /// `None` for unlimited.
    total_frames: Option<u64>,
    next: i64,
}

impl SourceFilter for VideoSource {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                width,
                height,
                time_base,
                frame_rate,
                sample_aspect_ratio,
                ..
            } = &mut out
            {
                *width = self.width;
                *height = self.height;
                *time_base = self.frame_rate.inverse();
                *frame_rate = self.frame_rate;
                *sample_aspect_ratio = self.sar;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_frames.is_some_and(|n| self.next as u64 >= n) {
            return Ok(None);
        }
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Yuv420p, self.width, self.height)?;
        frame.pts = Timestamp::new(self.next);
        frame.time_base = self.frame_rate.inverse();
        frame.set_duration_ticks(1);
        frame.sample_aspect_ratio = self.sar;
        self.next = self.next.saturating_add(1);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(self.next)
    }

    fn flush_state(&mut self) {
        self.next = 0;
    }
}

pub mod video {
    use super::{
        Constraint, FilterDesc, FilterFlags, FormatSet, Instance, Instantiate, MediaType,
        NodeFormats, Pad, PixFmt, Sourced, VideoOpts, VideoSource,
    };

    pub const DESC: FilterDesc = FilterDesc {
        name: "nullsrc",
        description: "Null video source, return unprocessed video frames",
        inputs: &[],
        outputs: &[Pad {
            name: "default",
            media_type: MediaType::Video,
        }],
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let opts = VideoOpts::parse(req.args)?;
        let (width, height) = opts.size;
        let rate = opts.rate.0;
        let total_frames = if opts.duration < vaco_core::Duration::ZERO {
            None
        } else {
            Some(crate::frame_budget(opts.duration, rate))
        };
        let source = VideoSource {
            width,
            height,
            frame_rate: rate,
            sar: opts.sar,
            total_frames,
            next: 0,
        };
        Ok(Instance {
            desc: DESC,
            formats: NodeFormats {
                inputs: Vec::new(),
                outputs: vec![FormatSet {
                    pixel_formats: Some(Constraint::OneOf(vec![PixFmt::Yuv420p])),
                    ..FormatSet::default()
                }],
                ties: Vec::new(),
                label: req.instance.to_owned(),
            },
            filter: Box::new(Sourced::new(source)),
        })
    }
}

// ------------------------------------------------------------- anullsrc

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "anullsrc", help = "null audio source")]
pub(crate) struct AudioOpts {
    #[opt(name = "channel_layout", alias = "cl", help = "set channel_layout", default = "stereo".to_owned(), flags(filtering))]
    pub channel_layout: String,
    #[opt(name = "sample_rate", alias = "r", help = "set sample rate", default = 44100, range = 1..=i32::MAX, flags(filtering))]
    pub sample_rate: i32,
    #[opt(name = "nb_samples", alias = "n", help = "set the number of samples per requested frame", default = 1024, range = 1..=65535, flags(filtering))]
    pub nb_samples: i32,
    #[opt(name = "duration", alias = "d", help = "set the audio duration", default = UNLIMITED, flags(filtering))]
    pub duration: VDuration,
}

impl AudioOpts {
    fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
        let mut o = Self::default();
        if let Some(text) = args {
            o.set_from_string(text, "=", ":")
                .map_err(|e| e.to_string())?;
        }
        Ok(o)
    }
}

#[derive(Debug)]
pub(crate) struct AudioSource {
    layout: vaco_chlayout::ChannelLayout,
    sample_rate: u32,
    block: u32,
    total_samples: Option<u64>,
    produced: u64,
}

impl SourceFilter for AudioSource {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Audio {
                sample_rate,
                layout,
                time_base,
                ..
            } = &mut out
            {
                *sample_rate = self.sample_rate;
                *layout = self.layout.clone();
                *time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.total_samples.is_some_and(|n| self.produced >= n) {
            return Ok(None);
        }
        let want = self.total_samples.map_or(self.block, |n| {
            u32::try_from(n - self.produced)
                .unwrap_or(self.block)
                .min(self.block)
        });
        let mut frame = ctx.pool().acquire_audio(
            vaco_sampfmt::SampleFmt::S16,
            self.layout.clone(),
            want,
            self.sample_rate,
        )?;
        frame.pts = Timestamp::new(i64::try_from(self.produced).unwrap_or(0));
        frame.time_base = Rational::new(1, i32::try_from(self.sample_rate.max(1)).unwrap_or(1));
        frame.set_duration_ticks(i64::from(want));
        self.produced = self.produced.saturating_add(u64::from(want));
        Ok(Some(frame))
    }

    fn end_pts(&self) -> Timestamp {
        Timestamp::new(i64::try_from(self.produced).unwrap_or(0))
    }

    fn flush_state(&mut self) {
        self.produced = 0;
    }
}

pub mod audio {
    use super::{
        AudioOpts, AudioSource, FilterDesc, FilterFlags, FormatSet, Instance, Instantiate,
        MediaType, NodeFormats, Pad, Sourced,
    };

    pub const DESC: FilterDesc = FilterDesc {
        name: "anullsrc",
        description: "Null audio source, return empty audio frames",
        inputs: &[],
        outputs: &[Pad {
            name: "default",
            media_type: MediaType::Audio,
        }],
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let opts = AudioOpts::parse(req.args)?;
        let layout = vaco_chlayout::ChannelLayout::from_name(&opts.channel_layout)
            .ok_or_else(|| format!("anullsrc: bad channel_layout `{}`", opts.channel_layout))?;
        let sample_rate = u32::try_from(opts.sample_rate.max(1)).unwrap_or(44100);
        let total_samples = if opts.duration < vaco_core::Duration::ZERO {
            None
        } else {
            Some(crate::sample_budget(opts.duration, sample_rate))
        };
        let source = AudioSource {
            layout,
            sample_rate,
            block: u32::try_from(opts.nb_samples.max(1)).unwrap_or(1024),
            total_samples,
            produced: 0,
        };
        Ok(Instance {
            desc: DESC,
            formats: NodeFormats {
                inputs: Vec::new(),
                outputs: vec![FormatSet::default()],
                ties: Vec::new(),
                label: req.instance.to_owned(),
            },
            filter: Box::new(Sourced::new(source)),
        })
    }
}
