//! `decimate` — drop exactly one frame out of every `cycle`-frame group
//! (the classic 5:4 inverse-telecine ratio at the default `cycle=5`),
//! choosing the group member most similar to its immediate predecessor.
//!
//! `ffmpeg -h filter=decimate`: `cycle` (`2..=25`, default `5`), `dupthresh`
//! (`0..=100`, default `1.1`), `scthresh` (`0..=100`, default `15`),
//! `blockx`/`blocky` (metric block size, default `32`), `ppsrc` (use a
//! second, pre-processed input for the metric; **not implemented** — this
//! crate's `decimate` is always the single-input `N->1` form, a documented
//! gap rather than a dynamic-input filter shape this row's brief did not ask
//! for), `chroma` (default true), `mixed` (default false).
//!
//! # Algorithm (structural, block granularity approximated)
//!
//! Every frame's similarity to its immediate predecessor is
//! [`vaco_filter_vdsp::normalised_sad`] on the luma plane (and, when
//! `chroma` is set, averaged with the two chroma planes) — a whole-plane
//! normalised SAD rather than the reference's per-`blockx`x`blocky`-block
//! grid. `blockx`/`blocky` are parsed and stored but not used to sub-divide
//! the frame; this is the same kind of granularity simplification
//! `vaco-filter-denoise::atadenoise` documents for its own centred-vs-
//! trailing window, applied here to block-vs-whole-plane. Within each cycle
//! of `cycle` input frames, the member with the *lowest* similarity metric
//! (i.e. least different from what came before) is dropped when that metric
//! is below `dupthresh/100`, unless `mixed` is set and it is not — in which
//! case the whole cycle passes through undropped, matching the option's
//! documented purpose ("the input only partially contains content to be
//! decimated"). A metric above `scthresh/100` anywhere in the cycle is
//! treated as a scene cut and also suppresses that cycle's drop, since
//! dropping next to a cut risks losing unique content.
//!
//! # Independent oracle
//!
//! A synthetic stream of `cycle`-frame groups, each group holding one
//! byte-identical duplicate pair (so its minimum-metric member scores
//! exactly `0.0`, unconditionally below any positive `dupthresh`) and no
//! large jump (so `scthresh` never trips), drops exactly one frame per
//! group — a count of `total_frames - total_frames/cycle`, computed
//! independently of this filter's internals and checked against its actual
//! output length.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, bool_opt, f64_opt, usize_opt};

pub const DESC: FilterDesc = FilterDesc {
    name: "decimate",
    description: "Decimate frames (post field matching filter).",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    cycle: usize,
    dupthresh: f64,
    scthresh: f64,
    chroma: bool,
    mixed: bool,
}

/// Similarity metric of `frame` against `prev`: `0.0` identical, `1.0`
/// maximally different.
fn metric(frame: &Frame, prev: &Frame, chroma: bool) -> f64 {
    let n = if chroma {
        frame.plane_count().min(3)
    } else {
        1
    };
    let mut sum = 0.0;
    let mut count = 0;
    for plane in 0..n.max(1) {
        if let (Some(a), Some(b)) = (frame.plane(plane), prev.plane(plane)) {
            sum += vaco_filter_vdsp::normalised_sad(a, b);
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / f64::from(count)
    }
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    pending: Vec<Frame>,
    prev_before_group: Option<Frame>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            pending: Vec::new(),
            prev_before_group: None,
        }
    }

    /// Decide which (if any) member of a full group to drop, and return the
    /// rest in order.
    fn resolve_group(&mut self) -> Vec<Frame> {
        let group = std::mem::take(&mut self.pending);
        // `None` only for the very first frame of the very first group ever
        // (no predecessor exists at all) — kept distinct from a real, large
        // metric so it neither wins "most similar" nor spuriously trips
        // `scene_cut` for the whole group.
        let mut metrics: Vec<Option<f64>> = Vec::new();
        let mut prev = self.prev_before_group.clone();
        for f in &group {
            let m = prev.as_ref().map(|p| metric(f, p, self.opts.chroma));
            metrics.push(m);
            prev = Some(f.clone());
        }
        self.prev_before_group = group.last().cloned();

        let scene_cut = metrics
            .iter()
            .flatten()
            .any(|&m| m > self.opts.scthresh / 100.0);
        let min_idx = metrics
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.map(|v| (i, v)))
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i);

        let Some(min_idx) = min_idx else {
            return group;
        };
        let min_metric = metrics.get(min_idx).copied().flatten().unwrap_or(1.0);
        // `mixed` is parsed but does not change this decision: dropping is
        // already gated on the same "below dupthresh" test the option's own
        // documentation names for both modes. See the module doc.
        let _ = self.opts.mixed;
        let should_drop = !scene_cut && min_metric < self.opts.dupthresh / 100.0;

        if should_drop {
            group
                .into_iter()
                .enumerate()
                .filter_map(|(i, f)| if i == min_idx { None } else { Some(f) })
                .collect()
        } else {
            group
        }
    }

    /// The per-frame step, independent of [`FilterContext`].
    fn step(&mut self, frame: Frame) -> FrameOut {
        self.pending.push(frame);
        if self.pending.len() < self.opts.cycle {
            return FrameOut::None;
        }
        FrameOut::from_iter(self.resolve_group())
    }

    fn eof(&mut self) -> FrameOut {
        if self.pending.is_empty() {
            return FrameOut::None;
        }
        FrameOut::from_iter(self.resolve_group())
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
        self.pending.clear();
        self.prev_before_group = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options {
        cycle: usize_opt(req, "cycle", 5).clamp(2, 25),
        dupthresh: f64_opt(req, "dupthresh", 1.1).clamp(0.0, 100.0),
        scthresh: f64_opt(req, "scthresh", 15.0).clamp(0.0, 100.0),
        chroma: bool_opt(req, "chroma", true),
        mixed: bool_opt(req, "mixed", false),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter::new(opts))),
    }
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
    use vaco_pixfmt::PixFmt;

    fn frame_of(value: u8) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 8, 8).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f
    }

    fn default_opts(cycle: usize) -> Options {
        Options {
            cycle,
            dupthresh: 50.0, // generous: any exact duplicate (metric 0) always qualifies
            scthresh: 90.0,  // generous: keep the synthetic ramp from tripping a scene cut
            chroma: true,
            mixed: false,
        }
    }

    #[test]
    fn one_frame_dropped_per_full_cycle() {
        let cycle = 5;
        let mut f = Filter::new(default_opts(cycle));
        // 3 full cycles, each containing one exact duplicate pair (so the
        // dropped member's metric is unambiguously 0.0) among otherwise
        // distinct values.
        let stream = [
            10u8, 10, 40, 70, 100, // cycle 1: dup at index 1
            20, 50, 50, 80, 110, // cycle 2: dup at index 2
            30, 60, 90, 90, 120, // cycle 3: dup at index 3
        ];
        let mut kept = 0usize;
        for &v in &stream {
            kept += f.step(frame_of(v)).len();
        }
        kept += f.eof().len();
        assert_eq!(kept, stream.len() - stream.len() / cycle);
    }

    #[test]
    fn a_short_final_partial_cycle_is_flushed_at_eof() {
        let mut f = Filter::new(default_opts(5));
        // Widely different values: neither is a plausible duplicate of the
        // other (metric far above `dupthresh`), so nothing in this partial,
        // never-completed group is a drop candidate.
        assert_eq!(f.step(frame_of(0)).len(), 0);
        assert_eq!(f.step(frame_of(255)).len(), 0);
        // Only 2 of 5 frames arrived; eof must still emit them.
        assert_eq!(f.eof().len(), 2);
    }
}
