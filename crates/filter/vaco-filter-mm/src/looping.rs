//! `loop`/`aloop` — replay a window of the stream after it ends.
//!
//! # Measured against the reference (ffmpeg 8.1): this is a *suffix*
//! looper, not an in-place one
//!
//! `ffmpeg -h filter=loop` documents `loop` (`-1..INT_MAX`, default `0`),
//! `size` (`0..32767` frames, default `0`), `start` (`-1..I64_MAX` frames,
//! default `0`) and `time` (a duration, used only when `start=-1`). `aloop`
//! is the same shape in samples: `size` is `0..INT_MAX`.
//!
//! The name invites the wrong mental model — that `loop=N` plays the whole
//! clip `N+1` times, or that it loops the `[start, start+size)` window *in
//! place* of the frames it covers. Neither is measured behaviour. Built and
//! confirmed with `ffmpeg -f lavfi -i "color=size=4x4:rate=5:duration=10"
//! -vf "trim=end_frame=5,setpts=PTS-STARTPTS,loop=loop=<N>:size=<S>:start=<T>,showinfo"`:
//!
//! | Args | Output frame count (input trimmed to exactly 5, pts `0..4`) |
//! |---|---|
//! | `loop=0:size=5:start=0` | 5 — identical to no filter at all |
//! | `loop=1:size=5:start=0` | 10 |
//! | `loop=2:size=5:start=0` | 15 |
//! | `loop=1:size=2:start=0` | 7 |
//! | `loop=1:size=2:start=1` | 7 |
//! | `loop=3:size=1:start=0` | 8, with pts `0..7` |
//!
//! Reading these together: **the entire input stream is always passed
//! through unchanged first**, and *then* the `[start, start+size)` window
//! (captured while it streamed past, not seeked back to) is appended after
//! it, `loop` more times — `size` extra frames per replay, regardless of
//! `start`'s value, and the replayed frames' PTS **continues the original
//! stream's arithmetic progression** (frame 5 in the `loop=3:size=1` row
//! gets pts `5`, not a PTS derived from the window frame's own original
//! pts). `loop=0` is a true no-op — measured identical to no filter present
//! — and `loop=-1` never stops appending, which is the one case this
//! module's `flush` legitimately never returns empty for; the reference's
//! own "loop the first frame forever" idiom
//! (`loop=loop=-1:size=1:start=0` on a one-frame input) is just this rule
//! applied to a stream whose "play through once" phase is a single frame.
//!
//! # Allocation: the window is bounded by [`vaco_limits::Budget`], not just
//! by `size`'s declared range
//!
//! `size`'s reference-documented range (`0..32767` frames for `loop`,
//! `0..INT_MAX` *samples* for `aloop`) is itself the conformance answer for
//! range validation (CONFORMANCE-FINDINGS 31) and is applied as an
//! `#[opt(range = ..)]` bound. But a frame count alone does not bound
//! memory: a `loop` window of 32767 frames at 7680x4320 4:4:4 16-bit is
//! multiple gigabytes, and `aloop`'s own declared range does not bound
//! sample count at all. So every frame this filter retains for the window
//! is charged against a [`vaco_limits::Budget`] (`Limits::permissive`, the
//! same default `vaco-filter-audio::aresample` uses for its own per-instance
//! meter) by its actual plane bytes — real, already-allocated frame data,
//! not a size computed from the option before any frame exists, which is
//! the specific pattern that produced `cellauto`'s 83 GB allocation. If a
//! charge fails, the window simply **stops growing early** rather than
//! erroring or panicking; the filter then loops whatever fraction of the
//! requested window it could afford. This is the explicit clamp: it never
//! rejects construction (the reference does not either), it silently caps
//! actual memory instead.
//!
//! # `aloop`'s `size`/`start`: frame-granularity, not sample-exact
//!
//! `atrim` cuts a straddling frame at the exact sample it needs to. This
//! filter does not: a frame is included in the window once the running
//! sample count reaches `start`, and the window stops admitting whole
//! frames once its own accumulated sample count reaches `size` — so the
//! window's true sample span can overshoot `size` by up to one frame's
//! worth, and `start` is rounded down to whichever frame boundary contains
//! it. Not measured against the reference's own sample-exact behaviour;
//! recorded here rather than silently approximated.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_limits::{Budget, Limits};

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];
const AUDIO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Audio,
}];

/// Sum of every plane's real byte length — the frame's actual allocated
/// size, not a size computed from an option.
fn frame_bytes(frame: &Frame) -> u64 {
    (0..8)
        .filter_map(|i| frame.plane(i))
        .map(|p| p.as_slice().len() as u64)
        .sum()
}

/// How many "units" one frame represents: 1 for video, its sample count for
/// audio — `size`/`start` are frames for `loop`, samples for `aloop`.
fn units(frame: &Frame, is_audio: bool) -> i64 {
    if !is_audio {
        return 1;
    }
    match &frame.data {
        FrameData::Audio { samples, .. } => i64::from(*samples),
        FrameData::Video { .. } => 0,
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Opts {
    pub loop_count: i64,
    pub size: i64,
    pub start: i64,
    pub time_secs: Option<f64>,
}

#[derive(Debug)]
pub(crate) struct Filter {
    is_audio: bool,
    loop_count: i64,
    size: i64,
    start: i64,
    start_time: Option<f64>,

    seen_units: i64,
    window: VecDeque<Frame>,
    window_units: i64,
    budget: Budget,
    budget_exhausted: bool,

    next_pts: Option<i64>,
    replay_index: usize,
    replays_done: i64,
}

impl Filter {
    fn new(opts: &Opts, is_audio: bool) -> Self {
        Self {
            is_audio,
            loop_count: opts.loop_count,
            size: opts.size,
            start: opts.start,
            start_time: opts.time_secs,
            seen_units: 0,
            window: VecDeque::new(),
            window_units: 0,
            budget: Budget::new(Limits::permissive()),
            budget_exhausted: false,
            next_pts: None,
            replay_index: 0,
            replays_done: 0,
        }
    }

    /// Whether the window has started admitting frames yet: either a plain
    /// frame-index `start`, or (`start == -1`) the first frame at or past
    /// `start_time` seconds.
    fn window_started(&self, frame: &Frame) -> bool {
        if self.start == -1 {
            let Some(threshold) = self.start_time else {
                return true;
            };
            let t = frame.pts.to_seconds(frame.time_base).unwrap_or(0.0);
            t >= threshold
        } else {
            self.seen_units >= self.start
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        let u = units(&frame, self.is_audio);
        let started = self.window_started(&frame);
        if started && self.size > 0 && self.window_units < self.size && !self.budget_exhausted {
            let bytes = frame_bytes(&frame);
            if self.budget.charge(bytes).is_ok() {
                self.window.push_back(frame.clone());
                self.window_units = self.window_units.saturating_add(u);
            } else {
                // The clamp: stop growing the window rather than reject
                // construction or panic. Whatever fit gets looped.
                self.budget_exhausted = true;
            }
        }
        self.seen_units = self.seen_units.saturating_add(u.max(1));
        let pts = frame.pts.ticks();
        let step = frame.duration.0.max(1);
        if let Some(p) = pts {
            self.next_pts = Some(p.saturating_add(step));
        }
        Ok(FrameOut::One(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        if self.window.is_empty() || self.loop_count == 0 {
            return Ok(FrameOut::None);
        }
        if self.loop_count >= 0 && self.replays_done >= self.loop_count {
            return Ok(FrameOut::None);
        }
        let Some(mut frame) = self.window.get(self.replay_index).cloned() else {
            return Ok(FrameOut::None);
        };
        let step = frame.duration.0.max(1);
        let pts = self.next_pts.unwrap_or(0);
        frame.pts = Timestamp::new(pts);
        self.next_pts = Some(pts.saturating_add(step));
        self.replay_index += 1;
        if self.replay_index >= self.window.len() {
            self.replay_index = 0;
            self.replays_done = self.replays_done.saturating_add(1);
        }
        Ok(FrameOut::One(frame))
    }

    fn flush_state(&mut self) {
        self.window.clear();
        self.window_units = 0;
        self.seen_units = 0;
        self.next_pts = None;
        self.replay_index = 0;
        self.replays_done = 0;
        self.budget = Budget::new(Limits::permissive());
        self.budget_exhausted = false;
    }
}

fn build(media: MediaType, desc: FilterDesc, opts: &Opts, req: &Instantiate<'_>) -> Instance {
    let filter = Filter::new(opts, media == MediaType::Audio);
    Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(filter)),
    }
}

pub mod video {
    use vaco_core::Duration as VDuration;

    #[allow(unused_imports, reason = "AUDIO_PAD is unused in this module's own build call")]
    use super::{
        AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Opts, Pad, VIDEO_PAD,
        build,
    };

    #[derive(Debug, Clone, vaco_opts::Options)]
    #[options(name = "loop", help = "loop video frames")]
    pub(crate) struct RawOpts {
        #[opt(name = "loop", help = "number of loops", default = 0, range = -1..=i32::MAX, flags(filtering))]
        pub loop_count: i32,
        #[opt(name = "size", help = "max number of frames to loop", default = 0_i64, range = 0..=32767, flags(filtering))]
        pub size: i64,
        #[opt(name = "start", help = "set the loop start frame", default = 0_i64, range = -1..=i64::MAX, flags(filtering))]
        pub start: i64,
        #[opt(name = "time", help = "set the loop start time", default = None, flags(filtering))]
        pub time: Option<VDuration>,
    }

    impl RawOpts {
        fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
            use vaco_opts::OptionsExt as _;
            let mut o = Self::default();
            if let Some(text) = args {
                o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
            }
            Ok(o)
        }
    }

    pub const DESC: FilterDesc = FilterDesc {
        name: "loop",
        description: "Loop video frames",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let raw = RawOpts::parse(req.args)?;
        let opts = Opts {
            loop_count: i64::from(raw.loop_count),
            size: raw.size,
            start: raw.start,
            // `VDuration` is microseconds; frame timestamps are compared in
            // seconds via `Timestamp::to_seconds`.
            time_secs: raw.time.map(|d| d.0 as f64 / 1_000_000.0),
        };
        Ok(build(MediaType::Video, DESC, &opts, req))
    }
}

pub mod audio {
    use vaco_core::Duration as VDuration;

    #[allow(unused_imports, reason = "VIDEO_PAD is unused in this module's own build call")]
    use super::{
        AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, Opts, Pad, VIDEO_PAD,
        build,
    };

    #[derive(Debug, Clone, vaco_opts::Options)]
    #[options(name = "aloop", help = "loop audio samples")]
    pub(crate) struct RawOpts {
        #[opt(name = "loop", help = "number of loops", default = 0, range = -1..=i32::MAX, flags(filtering))]
        pub loop_count: i32,
        #[opt(name = "size", help = "max number of samples to loop", default = 0_i64, range = 0..=2_147_483_647_i64, flags(filtering))]
        pub size: i64,
        #[opt(name = "start", help = "set the loop start sample", default = 0_i64, range = -1..=i64::MAX, flags(filtering))]
        pub start: i64,
        #[opt(name = "time", help = "set the loop start time", default = None, flags(filtering))]
        pub time: Option<VDuration>,
    }

    impl RawOpts {
        fn parse(args: Option<&str>) -> std::result::Result<Self, String> {
            use vaco_opts::OptionsExt as _;
            let mut o = Self::default();
            if let Some(text) = args {
                o.set_from_string(text, "=", ":").map_err(|e| e.to_string())?;
            }
            Ok(o)
        }
    }

    pub const DESC: FilterDesc = FilterDesc {
        name: "aloop",
        description: "Loop audio samples",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        let raw = RawOpts::parse(req.args)?;
        let opts = Opts {
            loop_count: i64::from(raw.loop_count),
            size: raw.size,
            start: raw.start,
            time_secs: raw.time.map(|d| d.0 as f64 / 1_000_000.0),
        };
        Ok(build(MediaType::Audio, DESC, &opts, req))
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
    use vaco_filter_core::mock::{gray_frame, gray_link, video_source_formats};
    use vaco_filter_core::{Graph, GraphStatus};

    /// Sends exactly 5 frames through `loop=<args>` and returns the pts
    /// sequence it produced.
    fn run(args: &str, n_in: i64) -> Vec<i64> {
        let req = Instantiate {
            name: "loop",
            instance: "loop",
            args: Some(args),
            arguments: &[],
        };
        let instance = video::create(&req).unwrap();
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let node = graph.add(instance.desc, instance.formats, instance.filter);
        let sink = graph.add_sink("out", MediaType::Video, vaco_filter_core::mock::any_video_sink("out"));
        graph.connect(src, 0, node, 0).unwrap();
        graph.connect(node, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(4, 4, tb)).unwrap();
        graph.configure().unwrap();
        for i in 0..n_in {
            graph.send(src, gray_frame(4, 4, i, 0)).unwrap();
        }
        graph.close_source(src, vaco_core::Timestamp::new(n_in)).unwrap();
        let mut out = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    while let Ok(f) = graph.recv(sink) {
                        out.push(f.pts.ticks().unwrap_or(-1));
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        out
    }

    /// `loop=0` is a true no-op: measured identical to no filter at all.
    #[test]
    fn loop_zero_is_a_no_op() {
        assert_eq!(run("loop=0:size=5:start=0", 5), vec![0, 1, 2, 3, 4]);
    }

    /// The distinguishing case from this module's doc: `loop=1:size=5`
    /// appends the whole window once — 10 frames total, not 5.
    #[test]
    fn loop_one_appends_the_window_once() {
        assert_eq!(run("loop=1:size=5:start=0", 5).len(), 10);
    }

    /// `loop=3:size=1:start=0` appends three copies of frame 0, and PTS
    /// keeps counting up rather than resetting — measured against the
    /// reference (`ffmpeg 8.1`): pts `0..7`, not `0..4` followed by `0,0,0`.
    #[test]
    fn replayed_frames_continue_the_pts_sequence() {
        assert_eq!(run("loop=3:size=1:start=0", 5), vec![0, 1, 2, 3, 4, 5, 6, 7]);
    }

    #[test]
    fn window_size_two_appends_exactly_two_frames_per_loop() {
        assert_eq!(run("loop=1:size=2:start=0", 5).len(), 7);
    }

    /// An empty window (`size=0`) loops nothing even if `loop` is set.
    #[test]
    fn zero_size_window_loops_nothing() {
        assert_eq!(run("loop=5:size=0:start=0", 5).len(), 5);
    }
}
