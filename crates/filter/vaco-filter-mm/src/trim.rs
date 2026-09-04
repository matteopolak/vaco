//! `trim`/`atrim` — pick one continuous section from the input.
//!
//! # Measured against the reference (ffmpeg 8.1)
//!
//! ```text
//! ffmpeg -f lavfi -i "testsrc=rate=10:duration=2" \
//!        -vf "trim=start=0.3:end=0.7,showinfo" -f null -
//! # kept: pts_time 0.3, 0.4, 0.5, 0.6 — dropped: 0.7 and later, and everything before 0.3
//! ```
//! The boundary is **half-open, `[start, end)`**: a frame exactly at `start`
//! is kept, a frame exactly at `end` is dropped. Confirmed independently for
//! `start_frame`/`end_frame` (`trim=start_frame=3:end_frame=7` keeps frame
//! indices 3..6 inclusive, four frames) — the same convention on both axes.
//!
//! `atrim` additionally **cuts a straddling frame exactly at the sample
//! boundary** rather than keeping or dropping it whole:
//!
//! ```text
//! ffmpeg -f lavfi -i "sine=sample_rate=100:duration=1" \
//!        -af "asetnsamples=n=20,atrim=start_sample=25:end_sample=45,ashowinfo" -f null -
//! # input frames are [0,20) [20,40) [40,60) ...
//! # output: frame 1 -> pts=25 nb_samples=15  (the [20,40) frame cut to [25,40))
//! #         frame 2 -> pts=40 nb_samples=5   (the [40,60) frame cut to [40,45))
//! ```
//! A frame straddling the boundary is neither wholly kept nor wholly dropped
//! for audio — it is cut. `trim` (video) has no equivalent, because a video
//! frame is a single instant with no width to cut.
//!
//! Neither filter rebases timestamps — the kept frames keep their *original*
//! PTS, exactly as the reference does (the classic `trim=...,setpts=PTS-STARTPTS`
//! pattern exists precisely because `trim` alone does not rebase).
//!
//! # What is implemented
//!
//! `start`/`end` (seconds), `start_pts`/`end_pts` (raw ticks), `duration`
//! (seconds, combined with whichever start bound is set), `start_frame`/
//! `end_frame` (video) and `start_sample`/`end_sample` (audio). A frame must
//! satisfy *every* bound that was actually set — a structural reading of
//! "multiple constraints narrow the kept range" that is not measured against
//! every combination the reference accepts.

use vaco_core::{Duration as VDuration, MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::{Frame, FrameData, FramePool};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "trim", help = "trim the input")]
pub(crate) struct Opts {
    #[opt(name = "start", help = "first timestamp to pass, in seconds", default = None, flags(filtering))]
    pub start: Option<VDuration>,
    #[opt(name = "end", help = "first timestamp to drop, in seconds", default = None, flags(filtering))]
    pub end: Option<VDuration>,
    #[opt(name = "start_pts", help = "first PTS to pass", default = None, flags(filtering))]
    pub start_pts: Option<i64>,
    #[opt(name = "end_pts", help = "first PTS to drop", default = None, flags(filtering))]
    pub end_pts: Option<i64>,
    #[opt(name = "duration", help = "maximum duration, in seconds", default = None, flags(filtering))]
    pub duration: Option<VDuration>,
    #[opt(name = "start_frame", help = "first frame index to pass (video)", default = None, flags(filtering))]
    pub start_frame: Option<i64>,
    #[opt(name = "end_frame", help = "first frame index to drop (video)", default = None, flags(filtering))]
    pub end_frame: Option<i64>,
    #[opt(name = "start_sample", help = "first sample index to pass (audio)", default = None, flags(filtering))]
    pub start_sample: Option<i64>,
    #[opt(name = "end_sample", help = "first sample index to drop (audio)", default = None, flags(filtering))]
    pub end_sample: Option<i64>,
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
}

/// A resolved `[start, end)` bound, in whatever unit the caller resolved it
/// to (link ticks for `trim`, sample index for `atrim`).
#[derive(Debug, Clone, Copy, Default)]
struct Bound {
    start: Option<i64>,
    end: Option<i64>,
}

impl Bound {
    fn contains(&self, value: i64) -> bool {
        self.start.is_none_or(|s| value >= s) && self.end.is_none_or(|e| value < e)
    }
}

// ------------------------------------------------------------------- trim

#[derive(Debug)]
pub(crate) struct VideoFilter {
    opts: Opts,
    pts_bound: Bound,
    frame_bound: Bound,
    n: i64,
}

impl FrameFilter for VideoFilter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        let tb = match ctx.input_link(0) {
            Some(LinkFormat::Video { time_base, .. }) => *time_base,
            _ => vaco_core::Rational::UNDEFINED,
        };
        self.pts_bound = resolve_video_bound(&self.opts, tb);
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let pts_ok = input.pts.ticks().is_none_or(|p| self.pts_bound.contains(p));
        let frame_ok = self.frame_bound.contains(self.n);
        self.n += 1;
        if pts_ok && frame_ok {
            Ok(FrameOut::One(input))
        } else {
            Ok(FrameOut::None)
        }
    }

    fn flush_state(&mut self) {
        self.n = 0;
    }
}

fn resolve_video_bound(opts: &Opts, tb: vaco_core::Rational) -> Bound {
    let start = opts
        .start_pts
        .or_else(|| opts.start.and_then(|d| d.to_ticks(tb)));
    let end = opts.end_pts.or_else(|| {
        opts.end.and_then(|d| d.to_ticks(tb)).or_else(|| {
            let start_ticks = start?;
            let dur = opts.duration?.to_ticks(tb)?;
            Some(start_ticks.saturating_add(dur))
        })
    });
    Bound { start, end }
}

pub mod video {
    use super::{
        Bound, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Opts, Pad, Simple,
        VideoFilter,
    };
    use vaco_filter_core::negotiate::NodeFormats;

    const VIDEO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Video,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "trim",
        description: "Pick one continuous section from the input, drop the rest",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let opts = Opts::parse(req.args)?;
        let frame_bound = Bound {
            start: opts.start_frame,
            end: opts.end_frame,
        };
        Ok(Instance {
            desc: DESC,
            formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
            filter: Box::new(Simple::new(VideoFilter {
                opts,
                pts_bound: Bound::default(),
                frame_bound,
                n: 0,
            })),
        })
    }
}

// ------------------------------------------------------------------ atrim

#[derive(Debug)]
pub(crate) struct AudioFilter {
    opts: Opts,
    bound: Option<Bound>,
    next_sample: i64,
}

impl AudioFilter {
    fn ensure_bound(&mut self, tb: vaco_core::Rational, sample_rate: u32) {
        if self.bound.is_some() {
            return;
        }
        let sample_tb = vaco_core::Rational::new(1, i32::try_from(sample_rate.max(1)).unwrap_or(1));
        let start = self
            .opts
            .start_sample
            .or(self.opts.start_pts)
            .or_else(|| self.opts.start.and_then(|d| d.to_ticks(sample_tb)));
        let end = self.opts.end_sample.or(self.opts.end_pts).or_else(|| {
            self.opts
                .end
                .and_then(|d| d.to_ticks(sample_tb))
                .or_else(|| {
                    let start_ticks = start?;
                    let dur = self.opts.duration?.to_ticks(sample_tb)?;
                    Some(start_ticks.saturating_add(dur))
                })
        });
        let _ = tb;
        self.bound = Some(Bound { start, end });
    }
}

impl FrameFilter for AudioFilter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Audio {
            time_base,
            sample_rate,
            ..
        }) = ctx.input_link(0)
        {
            self.ensure_bound(*time_base, *sample_rate);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let FrameData::Audio { samples, .. } = &input.data else {
            return Ok(FrameOut::One(input));
        };
        let frame_start = input.pts.ticks().unwrap_or(self.next_sample);
        let frame_len = i64::from(*samples);
        let frame_end = frame_start.saturating_add(frame_len);
        self.next_sample = frame_end;

        let Some(bound) = self.bound else {
            return Ok(FrameOut::One(input));
        };
        let lo = bound.start.unwrap_or(i64::MIN).max(frame_start);
        let hi = bound.end.unwrap_or(i64::MAX).min(frame_end);
        if lo >= hi {
            return Ok(FrameOut::None);
        }
        if lo == frame_start && hi == frame_end {
            return Ok(FrameOut::One(input));
        }
        let skip = usize::try_from(lo - frame_start).unwrap_or(0);
        let keep = usize::try_from(hi - lo).unwrap_or(0);
        let cut = slice_audio(&input, skip, keep, lo)?;
        Ok(FrameOut::One(cut))
    }

    fn flush_state(&mut self) {
        self.next_sample = 0;
    }
}

/// Cut `[skip, skip + keep)` samples out of `frame`, byte-exact, with no
/// format conversion — a plain sub-range copy, which is all `atrim` needs
/// and is cheaper and more exact than routing through the `f64` domain
/// `vaco-filter-audio` uses for filters that actually do arithmetic.
fn slice_audio(frame: &Frame, skip: usize, keep: usize, new_pts: i64) -> Result<Frame> {
    let FrameData::Audio {
        format,
        sample_rate,
        layout,
        ..
    } = &frame.data
    else {
        return Err(vaco_core::Error::InvalidData(
            "atrim given a non-audio frame",
        ));
    };
    let (fmt, rate, layout) = (*format, *sample_rate, layout.clone());
    let channels = usize::try_from(layout.channels.max(1)).unwrap_or(1);
    let pool = FramePool::default();
    let mut out = pool.acquire_audio(fmt, layout, u32::try_from(keep).unwrap_or(0), rate)?;

    let bytes_per_sample = fmt.bytes_per_sample();
    let per_sample = if fmt.is_planar() {
        bytes_per_sample
    } else {
        bytes_per_sample.saturating_mul(channels)
    };
    let skip_bytes = skip.saturating_mul(per_sample);
    let keep_bytes = keep.saturating_mul(per_sample);

    for i in 0..frame.plane_count() {
        let Some(src) = frame.plane(i) else { break };
        let src_bytes = src.as_slice();
        let Some(window) = src_bytes.get(skip_bytes..skip_bytes.saturating_add(keep_bytes)) else {
            continue;
        };
        if let Some(mut dst) = out.plane_mut(i)
            && let Some(row) = dst.row_mut(0)
        {
            let n = window.len().min(row.len());
            if let (Some(d), Some(s)) = (row.get_mut(..n), window.get(..n)) {
                d.copy_from_slice(s);
            }
        }
    }
    out.pts = vaco_core::Timestamp::new(new_pts);
    out.time_base = frame.time_base;
    out.set_duration_ticks(i64::try_from(keep).unwrap_or(0));
    Ok(out)
}

pub mod audio {
    use super::{
        AudioFilter, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Opts, Pad, Simple,
    };
    use vaco_filter_core::negotiate::NodeFormats;

    const AUDIO_PAD: &[Pad] = &[Pad {
        name: "default",
        media_type: MediaType::Audio,
    }];

    pub const DESC: FilterDesc = FilterDesc {
        name: "atrim",
        description: "Pick one continuous section from the input, drop the rest",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let opts = Opts::parse(req.args)?;
        Ok(Instance {
            desc: DESC,
            formats: NodeFormats::passthrough(1, 1, MediaType::Audio, req.instance),
            filter: Box::new(Simple::new(AudioFilter {
                opts,
                bound: None,
                next_sample: 0,
            })),
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_filter_core::mock::{audio_frame, audio_link, gray_frame, gray_link};
    use vaco_filter_core::negotiate::NodeFormats;
    use vaco_filter_core::{Graph, GraphStatus};

    /// `trim=start_frame=3:end_frame=7` on ten frames keeps indices 3..6 —
    /// the boundary measured against ffmpeg 8.1 in this module's doc.
    #[test]
    fn video_keeps_the_half_open_frame_range() {
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            vaco_filter_core::mock::video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let opts = Opts {
            start_frame: Some(3),
            end_frame: Some(7),
            ..Opts::default()
        };
        let filter = VideoFilter {
            opts: opts.clone(),
            pts_bound: Bound::default(),
            frame_bound: Bound {
                start: opts.start_frame,
                end: opts.end_frame,
            },
            n: 0,
        };
        let node = graph.add(
            video::DESC,
            NodeFormats::passthrough(1, 1, MediaType::Video, "trim"),
            Box::new(Simple::new(filter)),
        );
        let sink = graph.add_sink(
            "out",
            MediaType::Video,
            vaco_filter_core::mock::any_video_sink("out"),
        );
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        graph
            .set_source_format(src, gray_link(4, 4, vaco_core::Rational::new(1, 25)))
            .unwrap();
        graph.configure().unwrap();

        // 8, not 10: the source link's queue caps at `max_frames` (8 by
        // default), and sending more than that before the first `run()`
        // would report backpressure rather than exercising the boundary.
        for i in 0..8i64 {
            graph.send(src, gray_frame(4, 4, i, 0)).unwrap();
        }
        graph
            .close_source(src, vaco_core::Timestamp::new(8))
            .unwrap();

        let mut kept = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
            loop {
                match graph.recv(sink) {
                    Ok(frame) => kept.push(frame.pts.ticks().unwrap_or(-1)),
                    Err(vaco_core::Error::Eof | vaco_core::Error::NeedMoreInput) => break,
                    Err(e) => panic!("unexpected recv error: {e}"),
                }
            }
        }
        assert_eq!(kept, vec![3, 4, 5, 6]);
    }

    /// `atrim=start_sample=25:end_sample=45` against 20-sample input frames
    /// cuts the straddling frames exactly — the measurement in this module's
    /// doc, reproduced as an automated check: `[20,40)` becomes `pts=25,
    /// n=15` and `[40,60)` becomes `pts=40, n=5`.
    #[test]
    fn audio_cuts_straddling_frames_at_the_sample_boundary() {
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Audio,
            vaco_filter_core::mock::audio_source_formats("in", 100),
        );
        let opts = Opts {
            start_sample: Some(25),
            end_sample: Some(45),
            ..Opts::default()
        };
        let filter = AudioFilter {
            opts,
            bound: None,
            next_sample: 0,
        };
        let node = graph.add(
            audio::DESC,
            NodeFormats::passthrough(1, 1, MediaType::Audio, "atrim"),
            Box::new(Simple::new(filter)),
        );
        let sink = graph.add_sink(
            "out",
            MediaType::Audio,
            vaco_filter_core::mock::any_audio_sink("out"),
        );
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        graph.set_source_format(src, audio_link(100)).unwrap();
        graph.configure().unwrap();

        // Three 20-sample frames: [0,20) [20,40) [40,60).
        for k in 0..3i64 {
            graph.send(src, audio_frame(100, 20, k * 20)).unwrap();
        }
        graph
            .close_source(src, vaco_core::Timestamp::new(60))
            .unwrap();

        let mut out = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
            loop {
                match graph.recv(sink) {
                    Ok(frame) => {
                        let pts = frame.pts.ticks().unwrap_or(-1);
                        let FrameData::Audio { samples, .. } = frame.data else {
                            panic!("expected an audio frame");
                        };
                        out.push((pts, samples));
                    }
                    Err(vaco_core::Error::Eof | vaco_core::Error::NeedMoreInput) => break,
                    Err(e) => panic!("unexpected recv error: {e}"),
                }
            }
        }
        assert_eq!(out, vec![(25, 15), (40, 5)]);
    }
}
