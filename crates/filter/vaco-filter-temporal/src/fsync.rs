//! `fsync` — hold/duplicate/drop input frames (zero-order hold, like `fps`)
//! to match an explicit list of target timestamps read from a file.
//!
//! `ffmpeg -h filter=fsync`: one option, `file`/`f` (a path, default `""`).
//!
//! # The file format: probed, not confirmed
//!
//! `ffmpeg -hide_banner -f lavfi -i testsrc2 -vf fsync=file=<missing>`
//! fails with `No such file or directory` (ffmpeg 8.1, 2026-08-23) — so the
//! option genuinely opens and reads the named file — but every line format
//! tried against an *existing* file (plain integers `0/1/2`, decimal
//! seconds `0.0/0.2/0.4`) failed identically with `Failed to configure
//! output pad` / `Invalid data found when processing input`, which did not
//! disambiguate what the reference actually wants on each line. Reverse-
//! engineering the exact grammar was out of this pass's budget (see
//! `docs/filter/vaco-filter-temporal.md` for the probe transcript). This
//! implementation defines its own contract instead of guessing the
//! reference's: **one target timestamp in seconds per line**, blank lines
//! and lines starting with `#` ignored — a plain, documented format that
//! realises the option's stated purpose ("synchronize video frames from
//! external source using provided list of frame timestamps") even though it
//! is not proven byte-for-byte compatible with the reference's own file
//! reader. A file that fails to open is a clean [`vaco_core::Error`] at
//! creation, not a silent passthrough.
//!
//! # Algorithm (zero-order hold against an explicit timestamp list)
//!
//! Exactly `fps`'s hold/duplicate/drop shape (see
//! `vaco_filter_video_format::fps`'s module doc for the general pattern),
//! except the output grid is the file's own timestamp list rather than a
//! uniform `1/fps` spacing: one input frame is always held one arrival
//! behind, and on the next arrival it is emitted once for every target
//! timestamp from the last one produced up to (but not including) the new
//! frame's own timestamp.
//!
//! # Independent oracle
//!
//! A target list `[0.0, 0.0, 0.1]` (three targets, the first two
//! coinciding) against two input frames arriving at `0.0` and `0.1` must
//! duplicate the first input frame once (for both `0.0` targets) and then
//! emit the second for the final target — three outputs from two inputs,
//! predictable and countable exactly as `fps`'s upsampling case is.

use std::io::Read as _;

use vaco_core::{MediaType, Result, Timestamp};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, str_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "fsync",
    description: "Synchronize video frames from external source.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

/// Parse the target-timestamp file: see the module doc for the format this
/// crate defines (one seconds value per line).
pub(crate) fn parse_targets(text: &str) -> Vec<f64> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.parse::<f64>().ok())
        .collect()
}

#[derive(Debug)]
pub(crate) struct Filter {
    targets: Vec<f64>,
    next_target: usize,
    pending: Option<Frame>,
}

impl Filter {
    pub(crate) fn new(targets: Vec<f64>) -> Self {
        Self {
            targets,
            next_target: 0,
            pending: None,
        }
    }

    fn frame_seconds(frame: &Frame) -> f64 {
        let Some(ticks) = frame.pts.ticks() else {
            return 0.0;
        };
        #[allow(clippy::cast_precision_loss, reason = "display-scale timestamp conversion")]
        {
            ticks as f64 * f64::from(frame.time_base.num) / f64::from(frame.time_base.den.max(1))
        }
    }

    fn stamp(frame: &Frame, seconds: f64) -> Frame {
        let mut out = frame.clone();
        let tb = out.time_base;
        #[allow(clippy::cast_possible_truncation, reason = "display-scale timestamp conversion")]
        let ticks = (seconds * f64::from(tb.den.max(1)) / f64::from(tb.num.max(1))).round() as i64;
        out.pts = Timestamp::new(ticks);
        out
    }

    /// The hold/duplicate/drop step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        let arrival = Self::frame_seconds(&frame);
        let Some(held) = self.pending.replace(frame) else {
            return FrameOut::None;
        };
        let mut out: smallvec::SmallVec<[Frame; 4]> = smallvec::SmallVec::new();
        while let Some(&t) = self.targets.get(self.next_target) {
            if t >= arrival {
                break;
            }
            out.push(Self::stamp(&held, t));
            self.next_target = self.next_target.saturating_add(1);
        }
        FrameOut::from_iter(out)
    }

    fn eof(&mut self) -> FrameOut {
        let Some(held) = self.pending.take() else {
            return FrameOut::None;
        };
        let mut out: smallvec::SmallVec<[Frame; 4]> = smallvec::SmallVec::new();
        while let Some(&t) = self.targets.get(self.next_target) {
            out.push(Self::stamp(&held, t));
            self.next_target = self.next_target.saturating_add(1);
        }
        FrameOut::from_iter(out)
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        Ok(self.step(frame))
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        Ok(self.eof())
    }

    fn flush_state(&mut self) {
        self.pending = None;
        self.next_target = 0;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Result<Instance, String> {
    let path = str_opt(req, "file")
        .or_else(|| str_opt(req, "f"))
        .filter(|p| !p.is_empty())
        .ok_or_else(|| "fsync: `file` is required".to_owned())?;
    let mut text = String::new();
    std::fs::File::open(&path)
        .map_err(|e| format!("fsync: cannot open `{path}`: {e}"))?
        .read_to_string(&mut text)
        .map_err(|e| format!("fsync: cannot read `{path}`: {e}"))?;
    let targets = parse_targets(&text);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(targets))),
    })
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::integer_division,
    reason = "test code"
)]
mod tests {
    use super::*;
    use vaco_core::Rational;
    use vaco_pixfmt::PixFmt;

    fn frame_at(seconds: f64, tb: Rational) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        #[allow(clippy::cast_possible_truncation, reason = "test fixture")]
        let ticks = (seconds * f64::from(tb.den) / f64::from(tb.num)).round() as i64;
        f.pts = Timestamp::new(ticks);
        f.time_base = tb;
        f
    }

    #[test]
    fn parse_targets_skips_blanks_and_comments() {
        let targets = parse_targets("0.0\n\n# comment\n0.5\n1.0\n");
        assert_eq!(targets, vec![0.0, 0.5, 1.0]);
    }

    #[test]
    fn duplicate_targets_duplicate_the_held_frame() {
        let tb = Rational::new(1, 1000);
        let mut f = Filter::new(vec![0.0, 0.0, 0.1]);
        let mut count = 0usize;
        count += f.step(frame_at(0.0, tb)).len();
        count += f.step(frame_at(0.1, tb)).len();
        count += f.eof().len();
        assert_eq!(count, 3, "two coincident targets plus one more: 3 outputs from 2 inputs");
    }

    #[test]
    fn no_targets_produces_no_output() {
        let tb = Rational::new(1, 1000);
        let mut f = Filter::new(vec![]);
        let mut count = 0usize;
        count += f.step(frame_at(0.0, tb)).len();
        count += f.step(frame_at(0.1, tb)).len();
        count += f.eof().len();
        assert_eq!(count, 0);
    }
}
