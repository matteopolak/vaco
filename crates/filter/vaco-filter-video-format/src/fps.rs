//! `fps` — force a constant output frame rate by duplicating or dropping
//! frames (zero-order hold), never blending.
//!
//! `ffmpeg -h filter=fps` documents `fps` (default `"25"`), `start_time`
//! (default `DBL_MAX`, "use the first frame's own timestamp"), `round`
//! (`zero`/`inf`/`down`/`up`/`near`, default `near`) and `eof_action`
//! (`round`/`pass`, default `round`). All four implemented. Named rate
//! presets (`ntsc`, `pal`, `film`, `source`, `ntsc-film`) are **not**
//! implemented — `fps` is parsed as a plain rational
//! ([`vaco_core::parse::rational`]: an integer, a decimal, or a `num/den`
//! expression), so `fps=ntsc` is a clean parse error rather than `30000/1001`.
//!
//! # The algorithm (zero-order hold)
//!
//! Every input frame's timestamp is rescaled into the output's fixed-rate
//! grid (`out_tb = 1/fps`), rounded per `round`, giving that frame's *slot*.
//! One frame is always held one arrival behind: on the *next* frame's
//! arrival, the held frame is emitted once for every output slot from the
//! last one produced up to (but not including) the new frame's slot —
//! **duplicated** if that is more than one slot, **silently dropped
//! entirely** if the new frame's slot has not advanced past the last one
//! produced (two input frames landed in the same output instant). The
//! emitted frame's pixel data is the *held input frame's*, unchanged; its
//! timestamp is the **output grid's own**, not the input frame's original
//! one — see the measurement below.
//!
//! # Measured: duplicate on upsampling, and whose timestamp survives
//!
//! ```text
//! ffmpeg -f lavfi -i "testsrc2=size=8x8:rate=25:duration=0.12" \
//!   -vf "fps=50,showinfo" -f null -
//! ```
//!
//! Three 25fps input frames (`pts_time` 0, 0.04, 0.08) become **six** 50fps
//! output frames at `pts_time` 0, 0.02, 0.04, 0.06, 0.08, 0.1 — each input
//! frame duplicated exactly twice, and the printed `pts_time` values are the
//! *output* grid's regular spacing, not copies of any input frame's own
//! `pts_time`. The last input frame gets duplicated too, at end of stream:
//! seven arrivals would give five real emissions (2+2+1) if the last frame
//! were flushed only once, but six were observed, which is the next
//! measurement.
//!
//! # Measured: `eof_action=round`'s default extrapolates one more interval
//!
//! The gap between the second and third input frame's slots was 2 output
//! slots (`0.04s / 0.02s = 2`). At end of stream the reference emitted the
//! third (last-held) frame for **two** further slots, not one — i.e. it
//! assumed a *fourth* input frame would have arrived after the same gap and
//! filled the slots up to where that frame would have landed. `eof_action=
//! pass` is not itself measured; its documented contrast ("pass through last
//! frame" vs. "round similar to other frames") is implemented as the more
//! conservative reading — emit the held frame exactly once more.

use smallvec::SmallVec;
use vaco_core::{Duration, MediaType, Rational, Result, Rounding, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, LinkFormat, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "fps",
    description: "Force constant framerate",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EofAction {
    Round,
    Pass,
}

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "fps", help = "Force constant framerate")]
pub(crate) struct Opts {
    #[opt(
        name = "fps",
        help = "A string describing desired output framerate",
        default = "25".to_owned(),
        flags(video, filtering)
    )]
    pub fps: String,
    #[opt(
        name = "start_time",
        help = "Assume the first PTS should be this value",
        default = f64::MAX,
        flags(video, filtering)
    )]
    pub start_time: f64,
    #[opt(
        name = "round",
        help = "set rounding method for timestamps",
        default = "near".to_owned(),
        flags(video, filtering)
    )]
    pub round: String,
    #[opt(
        name = "eof_action",
        help = "action performed for last frame",
        default = "round".to_owned(),
        flags(video, filtering)
    )]
    pub eof_action: String,
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

fn parse_round(s: &str) -> std::result::Result<Rounding, String> {
    match s {
        "0" | "zero" => Ok(Rounding::Zero),
        "1" | "inf" => Ok(Rounding::Infinity),
        "2" | "down" => Ok(Rounding::Down),
        "3" | "up" => Ok(Rounding::Up),
        "5" | "near" => Ok(Rounding::NearestAwayFromZero),
        other => Err(format!("fps: bad `round` `{other}`")),
    }
}

fn parse_eof_action(s: &str) -> std::result::Result<EofAction, String> {
    match s {
        "0" | "round" => Ok(EofAction::Round),
        "1" | "pass" => Ok(EofAction::Pass),
        other => Err(format!("fps: bad `eof_action` `{other}`")),
    }
}

/// One frame held for possible duplication, plus the output slot it landed on.
#[derive(Debug)]
struct Pending {
    frame: Frame,
    slot: i64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    fps: Rational,
    out_tb: Rational,
    in_tb: Rational,
    start_time: Option<f64>,
    round: Rounding,
    eof_action: EofAction,
    pending: Option<Pending>,
    next_out_pts: i64,
    last_gap: i64,
    started: bool,
}

impl Filter {
    pub(crate) fn new(opts: &Opts) -> std::result::Result<Self, String> {
        let fps = vaco_core::parse::rational(&opts.fps)
            .ok_or_else(|| format!("fps: bad `fps` `{}`", opts.fps))?;
        if fps.num <= 0 || fps.den <= 0 {
            return Err(format!("fps: `fps` must be positive, got `{}`", opts.fps));
        }
        Ok(Self {
            fps,
            out_tb: fps.inverse(),
            in_tb: Rational::UNDEFINED,
            start_time: (opts.start_time < f64::MAX).then_some(opts.start_time),
            round: parse_round(&opts.round)?,
            eof_action: parse_eof_action(&opts.eof_action)?,
            pending: None,
            next_out_pts: 0,
            last_gap: 1,
            started: false,
        })
    }

    fn slot_of(&self, pts: Timestamp) -> i64 {
        pts.rescale(self.in_tb, self.out_tb, self.round)
            .ticks()
            .unwrap_or(self.next_out_pts)
    }

    fn stamp(&self, frame: &Frame, slot: i64) -> Frame {
        let mut out = frame.clone();
        out.pts = Timestamp::new(slot);
        out.time_base = self.out_tb;
        out.duration = Duration(1);
        out
    }
}

impl FrameFilter for Filter {
    fn configure(&mut self, ctx: &mut FilterContext<'_>) -> Result<()> {
        if let Some(LinkFormat::Video { time_base, .. }) = ctx.input_link(0) {
            self.in_tb = *time_base;
        }
        if let Some(mut out) = ctx.output_link(0).cloned() {
            if let LinkFormat::Video {
                time_base,
                frame_rate,
                ..
            } = &mut out
            {
                *time_base = self.out_tb;
                *frame_rate = self.fps;
            }
            ctx.set_output_link(0, out);
        }
        Ok(())
    }

    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        Ok(self.eof())
    }

    fn flush_state(&mut self) {
        self.pending = None;
        self.next_out_pts = 0;
        self.last_gap = 1;
        self.started = false;
    }
}

impl Filter {
    /// The hold/duplicate/drop step, independent of [`FilterContext`] so it
    /// can be exercised directly in tests without a full graph.
    fn step(&mut self, frame: Frame) -> FrameOut {
        let slot = self.slot_of(frame.pts);
        if !self.started {
            self.started = true;
            self.next_out_pts = self.start_time.map_or(slot, |t| {
                Rational::approximate(t, 1_000_000)
                    .checked_div(self.out_tb)
                    .map_or(slot, |r| r.to_f64().round() as i64)
            });
            self.pending = Some(Pending { frame, slot });
            return FrameOut::None;
        }
        let Some(held) = self.pending.take() else {
            self.pending = Some(Pending { frame, slot });
            return FrameOut::None;
        };
        self.last_gap = slot.saturating_sub(held.slot).max(1);
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        let mut n = self.next_out_pts;
        while n < slot {
            out.push(self.stamp(&held.frame, n));
            n = n.saturating_add(1);
        }
        self.next_out_pts = n.max(self.next_out_pts);
        self.pending = Some(Pending { frame, slot });
        FrameOut::from_iter(out)
    }

    /// The end-of-stream step: see the module doc's `eof_action` measurement.
    fn eof(&mut self) -> FrameOut {
        let Some(held) = self.pending.take() else {
            return FrameOut::None;
        };
        let span = match self.eof_action {
            EofAction::Round => self.last_gap.max(1),
            EofAction::Pass => 1,
        };
        let end = self.next_out_pts.saturating_add(span);
        let mut out: SmallVec<[Frame; 4]> = SmallVec::new();
        let mut n = self.next_out_pts;
        while n < end {
            out.push(self.stamp(&held.frame, n));
            n = n.saturating_add(1);
        }
        self.next_out_pts = end;
        FrameOut::from_iter(out)
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts)?;
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;
    use vaco_pixfmt::PixFmt;

    fn frame_at(pts: i64, tb: Rational) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        f.pts = Timestamp::new(pts);
        f.time_base = tb;
        f
    }

    fn opts(fps: &str) -> Opts {
        Opts {
            fps: fps.to_owned(),
            start_time: f64::MAX,
            round: "near".to_owned(),
            eof_action: "round".to_owned(),
        }
    }

    fn ticks_of(out: FrameOut, into: &mut Vec<i64>) {
        match out {
            FrameOut::None => {}
            FrameOut::One(fr) => into.extend(fr.pts.ticks()),
            FrameOut::Many(v) => into.extend(v.into_iter().filter_map(|fr| fr.pts.ticks())),
        }
    }

    /// Measured: 25->50 duplicates every input frame exactly twice, and the
    /// last frame's EOF flush duplicates it *again* (round extrapolates the
    /// same gap), for six total emissions from three input frames.
    #[test]
    fn upsample_duplicates_and_eof_extrapolates_the_last_gap() {
        let mut f = Filter::new(&opts("50")).unwrap();
        f.in_tb = Rational::new(1, 25);

        let mut ticks = Vec::new();
        for n in 0..3i64 {
            let input = frame_at(n, Rational::new(1, 25));
            ticks_of(f.step(input), &mut ticks);
        }
        ticks_of(f.eof(), &mut ticks);
        assert_eq!(ticks, vec![0, 1, 2, 3, 4, 5]);
    }

    /// Measured: 25->12.5 keeps roughly every other frame; consecutive input
    /// frames landing on the same output slot silently drop the earlier one
    /// rather than erroring.
    #[test]
    fn downsample_drops_frames_that_share_a_slot() {
        let mut f = Filter::new(&opts("25/2")).unwrap();
        f.in_tb = Rational::new(1, 25);

        let mut ticks = Vec::new();
        for n in 0..5i64 {
            let input = frame_at(n, Rational::new(1, 25));
            ticks_of(f.step(input), &mut ticks);
        }
        ticks_of(f.eof(), &mut ticks);
        // Every emitted slot must be distinct and monotonically increasing:
        // a downsample never emits the same output instant twice.
        for w in ticks.windows(2) {
            if let [a, b] = w {
                assert!(b > a, "slots must strictly increase: {ticks:?}");
            }
        }
        assert!(!ticks.is_empty());
    }

    #[test]
    fn eof_action_pass_emits_the_held_frame_only_once() {
        let mut f = Filter::new(&Opts {
            eof_action: "pass".to_owned(),
            ..opts("50")
        })
        .unwrap();
        f.in_tb = Rational::new(1, 25);

        let mut ticks = Vec::new();
        for n in 0..3i64 {
            ticks_of(f.step(frame_at(n, Rational::new(1, 25))), &mut ticks);
        }
        ticks_of(f.eof(), &mut ticks);
        // Round would extrapolate a further two slots (gap=2); pass emits
        // exactly one more, for five total instead of six.
        assert_eq!(ticks, vec![0, 1, 2, 3, 4]);
    }
}
