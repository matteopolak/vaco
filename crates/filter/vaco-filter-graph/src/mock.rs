//! A registry of worked filters, so the description language can be tested end
//! to end without a filter library.
//!
//! `vaco-filter-core` proved its traits with five mock filters and that caught
//! four design errors. The same approach here, chosen so that between them they
//! exercise everything the *graph* layer has to get right and a 1:1 filter
//! would leave untested:
//!
//! | Filter | Shape | Proves |
//! |---|---|---|
//! | `counter` | source | a chain with a closed upstream end |
//! | `null` / `anull` | 1:1 | the common case, and media typing |
//! | `invert` | 1:1, `gray8` only | negotiation against an exact format |
//! | `format` | 1:1, `pix_fmts=` list | **auto-conversion**, because two of them disagree |
//! | `split` | 1:N, N from options | pad counts that depend on arguments |
//! | `merge` | N:1, N from options | multi-input labels and `[a][b]f` |
//! | `scale` / `aresample` | converter | what the auto-conversion policy asks for |
//!
//! Not a filter library. Nothing here computes anything a real filter would.

use vaco_chlayout::ChannelLayout;
use vaco_core::{MediaType, Rational, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple, SourceFilter, Sourced};
use vaco_filter_core::negotiate::{Constraint, FormatSet, NodeFormats};
use vaco_filter_core::{Activity, Filter, FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_pixfmt::PixFmt;
use vaco_sampfmt::SampleFmt;

use crate::registry::{FilterRegistry, Instance, Instantiate, pads};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// A source of `n` grey frames.
#[derive(Debug)]
pub struct Counter {
    width: u32,
    height: u32,
    remaining: u64,
    next: i64,
}

impl SourceFilter for Counter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        // Built by mutating an unconfigured link rather than by naming
        // `ColorInfo`, which would cost this crate a dependency it needs
        // nowhere else.
        let mut format = LinkFormat::unconfigured(MediaType::Video);
        if let LinkFormat::Video {
            format: f,
            width,
            height,
            time_base,
            frame_rate,
            sample_aspect_ratio,
            ..
        } = &mut format
        {
            *f = PixFmt::Gray8;
            *width = self.width;
            *height = self.height;
            *time_base = Rational::new(1, 25);
            *frame_rate = Rational::new(25, 1);
            *sample_aspect_ratio = Rational::ONE;
        }
        ctx.set_output_link(0, format);
        Ok(())
    }

    fn produce(&mut self, ctx: &mut FilterContext<'_>) -> Result<Option<Frame>> {
        if self.remaining == 0 {
            return Ok(None);
        }
        self.remaining = self.remaining.saturating_sub(1);
        let pts = self.next;
        self.next = self.next.saturating_add(1);
        let mut frame = ctx
            .pool()
            .acquire_video(PixFmt::Gray8, self.width, self.height)?;
        frame.pts = vaco_core::Timestamp::new(pts);
        frame.time_base = Rational::new(1, 25);
        Ok(Some(frame))
    }

    fn end_pts(&self) -> vaco_core::Timestamp {
        vaco_core::Timestamp::new(self.next)
    }
}

/// A 1:1 filter that changes nothing.
#[derive(Debug, Default)]
pub struct Pass;

impl FrameFilter for Pass {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        Ok(FrameOut::One(input))
    }
}

/// A converter: re-issues each frame in the output link's negotiated format.
///
/// It converts no pixels — this is the graph layer's mock, and what it has to
/// prove is that the *right* converter was asked for with the *right* target
/// format, not that anyone can resample.
#[derive(Debug, Default)]
pub struct Reformat;

impl FrameFilter for Reformat {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let Some(out) = ctx.output_link(0).cloned() else {
            return Ok(FrameOut::One(input));
        };
        let mut frame = match out {
            LinkFormat::Video {
                format,
                width,
                height,
                ..
            } => ctx.pool().acquire_video(format, width, height)?,
            LinkFormat::Audio {
                format,
                sample_rate,
                ref layout,
                ..
            } => ctx
                .pool()
                .acquire_audio(format, layout.clone(), sample_rate, 1024)?,
        };
        frame.pts = input.pts;
        frame.duration = input.duration;
        frame.time_base = input.time_base;
        Ok(FrameOut::One(frame))
    }
}

/// A 1-in N-out filter: the first output gets the frame, the rest get copies.
#[derive(Debug)]
pub struct Split {
    outputs: usize,
}

impl Filter for Split {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if (0..self.outputs).any(|p| !ctx.output_has_room(p)) {
            if ctx.output_closed(0) {
                return Ok(Activity::Eof);
            }
            return Ok(Activity::Blocked);
        }
        if let Some(frame) = ctx.take_input(0) {
            for pad in 0..self.outputs {
                ctx.push_output(pad, frame.clone())?;
            }
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }
}

/// An N-in 1-out filter: forwards input 0 and discards the rest.
///
/// Deliberately *not* a frame synchroniser — that is
/// `vaco-filter-framesync`'s, and a graph test only needs the pad topology.
#[derive(Debug)]
pub struct Merge {
    inputs: usize,
}

impl Filter for Merge {
    fn activate(&mut self, ctx: &mut FilterContext<'_>) -> Result<Activity> {
        if !ctx.output_has_room(0) {
            return Ok(if ctx.output_closed(0) {
                Activity::Eof
            } else {
                Activity::Blocked
            });
        }
        let mut moved = false;
        for pad in 1..self.inputs {
            while ctx.take_input(pad).is_some() {
                moved = true;
            }
        }
        if let Some(frame) = ctx.take_input(0) {
            ctx.push_output(0, frame)?;
            return Ok(Activity::Progressed);
        }
        if ctx.input_at_eof(0) {
            ctx.close_all_outputs();
            return Ok(Activity::Eof);
        }
        if moved {
            return Ok(Activity::Progressed);
        }
        ctx.forward_wanted();
        Ok(Activity::NeedInput)
    }
}

/// The registry the tests and the fuzz target use.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockRegistry;

impl MockRegistry {
    /// A registry of the worked filters above.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

/// Every name [`MockRegistry`] answers to.
pub const NAMES: &[&str] = &[
    "counter",
    "null",
    "anull",
    "invert",
    "format",
    "aformat",
    "split",
    "merge",
    "amerge",
    "scale",
    "aresample",
];

fn number(req: &Instantiate<'_>, key: &str, default: usize) -> core::result::Result<usize, String> {
    let Some(v) = req.named(key).or_else(|| req.positional(0)) else {
        return Ok(default);
    };
    let n: usize = v
        .trim()
        .parse()
        .map_err(|_| format!("{key}: '{v}' is not a number"))?;
    if n == 0 || n > pads::MAX {
        return Err(format!(
            "{key} must be between 1 and {}, got {n}",
            pads::MAX
        ));
    }
    Ok(n)
}

fn pixel_formats(spec: &str) -> core::result::Result<Vec<PixFmt>, String> {
    spec.split('|')
        .filter(|s| !s.is_empty())
        .map(|s| PixFmt::from_name(s).map_err(|_| format!("unknown pixel format '{s}'")))
        .collect()
}

fn sample_formats(spec: &str) -> core::result::Result<Vec<SampleFmt>, String> {
    spec.split('|')
        .filter(|s| !s.is_empty())
        .map(|s| SampleFmt::from_name(s).map_err(|_| format!("unknown sample format '{s}'")))
        .collect()
}

impl FilterRegistry for MockRegistry {
    fn names(&self) -> Vec<&str> {
        NAMES.to_vec()
    }

    fn create(&self, req: &Instantiate<'_>) -> core::result::Result<Instance, String> {
        match req.name {
            "counter" => {
                let n = req
                    .named("n")
                    .or_else(|| req.positional(0))
                    .map_or(Ok(1u64), |v| {
                        v.trim().parse::<u64>().map_err(|_| format!("n: '{v}'"))
                    })?;
                Ok(Instance {
                    desc: FilterDesc {
                        name: "counter",
                        description: "numbered grey frames",
                        inputs: &[],
                        outputs: VIDEO_PAD,
                        flags: FilterFlags::empty(),
                    },
                    formats: NodeFormats {
                        inputs: Vec::new(),
                        outputs: vec![FormatSet::video_exact(PixFmt::Gray8)],
                        ties: Vec::new(),
                        label: String::new(),
                    },
                    filter: Box::new(Sourced::new(Counter {
                        width: 16,
                        height: 16,
                        remaining: n,
                        next: 0,
                    })),
                })
            }
            "null" | "invert" => {
                let formats = if req.name == "invert" {
                    NodeFormats::uniform(
                        1,
                        1,
                        MediaType::Video,
                        &FormatSet::video_exact(PixFmt::Gray8),
                        "",
                    )
                } else {
                    NodeFormats::passthrough(1, 1, MediaType::Video, "")
                };
                Ok(Instance {
                    desc: FilterDesc {
                        name: if req.name == "invert" {
                            "invert"
                        } else {
                            "null"
                        },
                        description: "pass video through",
                        inputs: VIDEO_PAD,
                        outputs: VIDEO_PAD,
                        flags: FilterFlags::TIMELINE_GENERIC,
                    },
                    formats,
                    filter: Box::new(Simple::new(Pass)),
                })
            }
            "anull" => Ok(Instance {
                desc: FilterDesc {
                    name: "anull",
                    description: "pass audio through",
                    inputs: AUDIO_PAD,
                    outputs: AUDIO_PAD,
                    flags: FilterFlags::empty(),
                },
                formats: NodeFormats::passthrough(1, 1, MediaType::Audio, ""),
                filter: Box::new(Simple::new(Pass)),
            }),
            "format" => {
                let list = req
                    .named("pix_fmts")
                    .or_else(|| req.positional(0))
                    .unwrap_or_default();
                let formats = pixel_formats(&list)?;
                let set = if formats.is_empty() {
                    FormatSet::default()
                } else {
                    FormatSet::video_list(formats)
                };
                Ok(Instance {
                    desc: FilterDesc {
                        name: "format",
                        description: "constrain the pixel format",
                        inputs: VIDEO_PAD,
                        outputs: VIDEO_PAD,
                        flags: FilterFlags::empty(),
                    },
                    formats: NodeFormats::uniform(1, 1, MediaType::Video, &set, ""),
                    filter: Box::new(Simple::new(Pass)),
                })
            }
            "aformat" => {
                let list = req
                    .named("sample_fmts")
                    .or_else(|| req.positional(0))
                    .unwrap_or_default();
                let formats = sample_formats(&list)?;
                let mut set = FormatSet::default();
                if !formats.is_empty() {
                    set.sample_formats = Some(Constraint::OneOf(formats).normalised());
                }
                if let Some(rate) = req.named("sample_rates") {
                    let r: u32 = rate
                        .trim()
                        .parse()
                        .map_err(|_| format!("sample_rates: '{rate}'"))?;
                    set.sample_rates = Some(Constraint::Exact(r));
                }
                set.channel_layouts = Some(Constraint::Exact(ChannelLayout::STEREO));
                Ok(Instance {
                    desc: FilterDesc {
                        name: "aformat",
                        description: "constrain the sample format",
                        inputs: AUDIO_PAD,
                        outputs: AUDIO_PAD,
                        flags: FilterFlags::empty(),
                    },
                    formats: NodeFormats::uniform(1, 1, MediaType::Audio, &set, ""),
                    filter: Box::new(Simple::new(Pass)),
                })
            }
            "split" => {
                let n = number(req, "outputs", 2)?;
                let outputs = pads::video(n).ok_or("too many outputs")?;
                Ok(Instance {
                    desc: FilterDesc {
                        name: "split",
                        description: "duplicate a video stream",
                        inputs: VIDEO_PAD,
                        outputs,
                        flags: FilterFlags::DYNAMIC_OUTPUTS,
                    },
                    formats: NodeFormats::passthrough(1, n, MediaType::Video, ""),
                    filter: Box::new(Split { outputs: n }),
                })
            }
            "merge" | "amerge" => {
                let n = number(req, "inputs", 2)?;
                let media = if req.name == "amerge" {
                    MediaType::Audio
                } else {
                    MediaType::Video
                };
                let inputs = pads::of(media, n).ok_or("too many inputs")?;
                let outputs = if media == MediaType::Audio {
                    AUDIO_PAD
                } else {
                    VIDEO_PAD
                };
                Ok(Instance {
                    desc: FilterDesc {
                        name: if media == MediaType::Audio {
                            "amerge"
                        } else {
                            "merge"
                        },
                        description: "combine several streams",
                        inputs,
                        outputs,
                        flags: FilterFlags::DYNAMIC_INPUTS,
                    },
                    formats: NodeFormats::passthrough(n, 1, media, ""),
                    filter: Box::new(Merge { inputs: n }),
                })
            }
            "scale" => Ok(Instance {
                desc: FilterDesc {
                    name: "scale",
                    description: "convert pixel format",
                    inputs: VIDEO_PAD,
                    outputs: VIDEO_PAD,
                    flags: FilterFlags::empty(),
                },
                formats: NodeFormats::converter(FormatSet::default(), FormatSet::default(), ""),
                filter: Box::new(Simple::new(Reformat)),
            }),
            "aresample" => Ok(Instance {
                desc: FilterDesc {
                    name: "aresample",
                    description: "convert sample format, rate and layout",
                    inputs: AUDIO_PAD,
                    outputs: AUDIO_PAD,
                    flags: FilterFlags::empty(),
                },
                formats: NodeFormats::converter(FormatSet::default(), FormatSet::default(), ""),
                filter: Box::new(Simple::new(Reformat)),
            }),
            other => Err(format!("no such filter: '{other}'")),
        }
    }
}
