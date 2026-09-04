//! `freezeframes` — replace a run of `source` frames with one frame taken
//! from a second `replace` input.
//!
//! `ffmpeg -h filter=freezeframes`: two video inputs named `source` and
//! `replace` (measured pad names); options `first`,
//! `last` (inclusive 0-based `source` frame index range to replace, both
//! default `0`), `replace` (which frame of the `replace` input to use,
//! default `0`).
//!
//! This is this row's one multi-input filter (`VV->V` in `ffmpeg
//! -filters`), so it goes through `vaco-filter-framesync`'s
//! [`FrameSyncFilter`]/[`Synced`] rather than a hand-rolled two-pad `Filter`,
//! per this crate's reuse rule — [`FsInput::dual`] (input 0 drives, input 1
//! is sampled) is exactly `overlay`/`blend`/`lut2`'s shape and fits this
//! filter too.
//!
//! # Picking "the frame at index `replace`" through a time-aligned sync
//!
//! `FrameSync` samples input 1 by timestamp proximity to each input-0 event,
//! not by an absolute frame counter — there is no framesync primitive for
//! "the Nth frame of stream 2" directly. This implementation reconstructs it
//! by watching input 1's sampled frame across events and counting each time
//! it visibly *changes* (by `pts`): the frame in place the `replace`-th time
//! it changes is cached and reused for every `source` frame in
//! `first..=last`. That is exact for the common case this filter is built
//! for — a `replace` input that holds one still frame per "chapter" — and a
//! documented structural approximation otherwise (see
//! `docs/filter/vaco-filter-temporal.md`).
//!
//! # Independent oracle
//!
//! With `first=1, last=2, replace=0` and a `source` stream of five distinct
//! frames and a `replace` stream holding one frame throughout, the output
//! must be `[source[0], replace[0], replace[0], source[3], source[4]]` —
//! computed directly from the inputs, not from re-running this filter's own
//! index bookkeeping a second way.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::FrameOut;
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;

use vaco_filter_framesync::{FrameSyncFilter, FrameSyncOpts, FsInput, Synced};
use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::i64_opt;

const INPUT_PADS: &[Pad] = &[
    Pad {
        name: "source",
        media_type: MediaType::Video,
    },
    Pad {
        name: "replace",
        media_type: MediaType::Video,
    },
];

const OUTPUT_PADS: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "freezeframes",
    description: "Freeze video frames.",
    inputs: INPUT_PADS,
    outputs: OUTPUT_PADS,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Options {
    first: i64,
    last: i64,
    replace: i64,
}

#[derive(Debug)]
pub(crate) struct Filter {
    opts: Options,
    main_index: i64,
    last_secondary_pts: Option<vaco_core::Timestamp>,
    secondary_change_count: i64,
    replacement: Option<Frame>,
}

impl Filter {
    pub(crate) fn new(opts: Options) -> Self {
        Self {
            opts,
            main_index: 0,
            last_secondary_pts: None,
            secondary_change_count: 0,
            replacement: None,
        }
    }

    fn observe_secondary(&mut self, secondary: Option<&Frame>) {
        let Some(secondary) = secondary else { return };
        if self.last_secondary_pts != Some(secondary.pts) {
            self.last_secondary_pts = Some(secondary.pts);
            if self.secondary_change_count == self.opts.replace {
                self.replacement = Some(secondary.clone());
            }
            self.secondary_change_count = self.secondary_change_count.saturating_add(1);
        } else if self.replacement.is_none() && self.opts.replace == 0 {
            // The replace input's very first frame, seen before any "change"
            // has been counted yet: `replace=0` means exactly this one.
            self.replacement = Some(secondary.clone());
        }
    }

    /// One aligned `(source, replace)` pair, independent of
    /// [`FilterContext`].
    fn on_pair(&mut self, main: Frame, secondary: Option<&Frame>) -> FrameOut {
        self.observe_secondary(secondary);
        let idx = self.main_index;
        self.main_index = self.main_index.saturating_add(1);
        if idx >= self.opts.first
            && idx <= self.opts.last
            && let Some(replacement) = &self.replacement
        {
            let mut out = replacement.clone();
            out.pts = main.pts;
            out.time_base = main.time_base;
            out.duration = main.duration;
            return FrameOut::One(out);
        }
        FrameOut::One(main)
    }
}

impl FrameSyncFilter for Filter {
    fn on_event(
        &mut self,
        _ctx: &mut FilterContext<'_>,
        event: &mut vaco_filter_framesync::FrameSyncEvent<'_>,
    ) -> Result<FrameOut> {
        let secondary = event.get(1).cloned();
        let Some(main) = event.take(0) else {
            return Ok(FrameOut::None);
        };
        Ok(self.on_pair(main, secondary.as_ref()))
    }

    fn inputs(&self, n: usize) -> Vec<FsInput> {
        FsInput::dual(n)
    }

    fn opts(&self) -> FrameSyncOpts {
        FrameSyncOpts::default()
    }

    fn flush_state(&mut self) {
        self.main_index = 0;
        self.last_secondary_pts = None;
        self.secondary_change_count = 0;
        self.replacement = None;
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let opts = Options {
        first: i64_opt(req, "first", 0).max(0),
        last: i64_opt(req, "last", 0).max(0),
        replace: i64_opt(req, "replace", 0).max(0),
    };
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(2, 1, MediaType::Video, req.instance),
        filter: Box::new(Synced::new(Filter::new(opts))),
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
    use vaco_core::Timestamp;
    use vaco_pixfmt::PixFmt;

    fn frame_of(value: u8, pts: i64) -> Frame {
        let pool = vaco_frame::FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 1, 1).unwrap();
        if let Some(mut p) = f.plane_mut(0) {
            p.fill(value);
        }
        f.pts = Timestamp::new(pts);
        f
    }

    fn sample(f: &Frame) -> u8 {
        f.plane(0).unwrap().row(0).unwrap()[0]
    }

    #[test]
    fn replaces_exactly_the_configured_index_range() {
        let opts = Options {
            first: 1,
            last: 2,
            replace: 0,
        };
        let mut f = Filter::new(opts);
        let replace_frame = frame_of(255, 0);
        let sources = [
            frame_of(10, 0),
            frame_of(20, 1),
            frame_of(30, 2),
            frame_of(40, 3),
            frame_of(50, 4),
        ];
        let mut out = Vec::new();
        for src in sources {
            let FrameOut::One(fr) = f.on_pair(src, Some(&replace_frame)) else {
                panic!("expected exactly one frame")
            };
            out.push(sample(&fr));
        }
        assert_eq!(out, vec![10, 255, 255, 40, 50]);
    }

    #[test]
    fn no_replace_input_yet_passes_the_source_through() {
        let opts = Options {
            first: 0,
            last: 4,
            replace: 0,
        };
        let mut f = Filter::new(opts);
        let FrameOut::One(fr) = f.on_pair(frame_of(7, 0), None) else {
            panic!("expected exactly one frame")
        };
        assert_eq!(sample(&fr), 7, "no replacement frame observed yet");
    }
}
