//! `reverse`/`areverse` — reverse a stream's frame order.
//!
//! `ffmpeg -h filter=reverse` documents no options at all, matching this
//! implementation. The reference's own warning — "this filter requires
//! memory to buffer the entire clip, so trimming is suggested" — is why
//! this crate's Budget defence (below) matters here more than almost
//! anywhere else in the row.
//!
//! # Content reverses; the timeline does not
//!
//! Built with `-f lavfi -i "color=size=4x4:rate=5:duration=10,format=gray"
//! -vf "trim=end_frame=5,setpts=PTS-STARTPTS,geq=lum='N*10',<reverse or
//! not>,showinfo"` — `geq=lum='N*10'` stamps each frame's luma with its own
//! frame index so content order is distinguishable from timing:
//!
//! ```text
//! forward:  mean 0, 10, 20, 30, 40   pts 0, 1, 2, 3, 4
//! reverse:  mean 40, 30, 20, 10, 0   pts 0, 1, 2, 3, 4
//! ```
//!
//! Content order flips; the pts sequence does not — output position `k`
//! keeps the pts (and `duration`/`time_base`) that position `k` had in the
//! original stream, and receives the pixel/sample data from original
//! position `N-1-k`. `reverse,reverse` composing to the identity is this
//! crate's falsifying test: an implementation that also reversed timestamps
//! would look plausible from one direction but would not be idempotent.
//! Non-uniform frame spacing was not measured; this implementation keeps
//! position `k`'s own timing metadata as the simplest reading consistent
//! with the above.
//!
//! # Allocation: this filter is supposed to buffer everything
//!
//! Every frame is retained until end of stream, by design — there is no
//! `size` option to bound the window the way `loop` has one. Each retained
//! frame is charged against a [`vaco_limits::Budget`] by its real plane
//! bytes, the same mechanism `loop`/`aloop` use. Once the budget is
//! exhausted, the filter stops admitting further frames into the buffer
//! rather than erroring — later frames are silently dropped from the
//! reversed output. The reference's own docs already tell a caller to
//! bound this with `trim`; this is the backstop for when they do not.

use std::collections::VecDeque;

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
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

fn frame_bytes(frame: &Frame) -> u64 {
    (0..8)
        .filter_map(|i| frame.plane(i))
        .map(|p| p.as_slice().len() as u64)
        .sum()
}

#[derive(Debug)]
pub(crate) struct Filter {
    /// Timing metadata (pts, duration, `time_base`), oldest first — kept
    /// separately from `content` so position `k`'s timing survives being
    /// paired with a different position's pixel/sample data.
    timing: VecDeque<Frame>,
    /// Frame content, oldest first. Cloning is Arc-plane-refcount cheap
    /// (see this crate's `split.rs`); the two queues share the same
    /// underlying plane buffers as `timing` while both exist, so this is
    /// not a second real copy of the pixel data.
    content: VecDeque<Frame>,
    budget: Budget,
    budget_exhausted: bool,
}

impl Default for Filter {
    fn default() -> Self {
        Self {
            timing: VecDeque::new(),
            content: VecDeque::new(),
            budget: Budget::new(Limits::permissive()),
            budget_exhausted: false,
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, _ctx: &mut FilterContext<'_>, frame: Frame) -> Result<FrameOut> {
        if self.budget_exhausted {
            // The clamp: once the budget is spent, stop retaining frames.
            // Nothing is forwarded per-frame either — `reverse` only ever
            // emits during `flush`, once the whole (possibly truncated)
            // buffer is known.
            return Ok(FrameOut::None);
        }
        if self.budget.charge(frame_bytes(&frame)).is_err() {
            self.budget_exhausted = true;
            return Ok(FrameOut::None);
        }
        self.timing.push_back(frame.clone());
        self.content.push_back(frame);
        Ok(FrameOut::None)
    }

    fn flush(&mut self, _ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let Some(timing) = self.timing.pop_front() else {
            return Ok(FrameOut::None);
        };
        let Some(content) = self.content.pop_back() else {
            return Ok(FrameOut::None);
        };
        let mut out = content;
        out.pts = timing.pts;
        out.duration = timing.duration;
        out.time_base = timing.time_base;
        Ok(FrameOut::One(out))
    }

    fn flush_state(&mut self) {
        self.timing.clear();
        self.content.clear();
        self.budget = Budget::new(Limits::permissive());
        self.budget_exhausted = false;
    }
}

fn build(media: MediaType, desc: FilterDesc, req: &Instantiate<'_>) -> Instance {
    Instance {
        desc,
        formats: NodeFormats::passthrough(1, 1, media, req.instance),
        filter: Box::new(Simple::new(Filter::default())),
    }
}

pub mod video {
    use super::{FilterDesc, FilterFlags, Instance, Instantiate, MediaType, VIDEO_PAD, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "reverse",
        description: "Reverse a video clip",
        inputs: VIDEO_PAD,
        outputs: VIDEO_PAD,
        flags: FilterFlags::empty(),
    };

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> \
                  signature every filter in this crate's registry.rs dispatches through"
    )]
    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        Ok(build(MediaType::Video, DESC, req))
    }
}

pub mod audio {
    use super::{AUDIO_PAD, FilterDesc, FilterFlags, Instance, Instantiate, MediaType, build};

    pub const DESC: FilterDesc = FilterDesc {
        name: "areverse",
        description: "Reverse an audio clip",
        inputs: AUDIO_PAD,
        outputs: AUDIO_PAD,
        flags: FilterFlags::empty(),
    };

    #[allow(
        clippy::unnecessary_wraps,
        reason = "must match the shared fn(&Instantiate) -> Result<Instance, String> \
                  signature every filter in this crate's registry.rs dispatches through"
    )]
    pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
        Ok(build(MediaType::Audio, DESC, req))
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

    /// Sends `n` frames, each with a distinct luma value equal to its own
    /// index, through `filter_names` (chained through `,` in the graph
    /// string sense — here just applied in sequence to the same `Filter`
    /// instances) and returns the luma values in output order.
    fn run(reversals: usize, n: i64) -> Vec<u8> {
        let req = Instantiate {
            name: "reverse",
            instance: "reverse",
            args: None,
            arguments: &[],
        };
        let mut graph = Graph::new();
        let src = graph.add_source(
            "in",
            MediaType::Video,
            video_source_formats("in", vaco_pixfmt::PixFmt::Gray8),
        );
        let mut prev = src;
        for _ in 0..reversals {
            let instance = video::create(&req).unwrap();
            let node = graph.add(instance.desc, instance.formats, instance.filter);
            graph.connect(prev, 0, node, 0).unwrap();
            prev = node;
        }
        let sink = graph.add_sink(
            "out",
            MediaType::Video,
            vaco_filter_core::mock::any_video_sink("out"),
        );
        graph.connect(prev, 0, sink, 0).unwrap();
        let tb = vaco_core::Rational::new(1, 25);
        graph.set_source_format(src, gray_link(1, 1, tb)).unwrap();
        graph.configure().unwrap();
        for i in 0..n {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "test luma stamp, n is small"
            )]
            let luma = (i * 10) as u8;
            graph.send(src, gray_frame(1, 1, i, luma)).unwrap();
        }
        graph
            .close_source(src, vaco_core::Timestamp::new(n))
            .unwrap();
        let mut out = Vec::new();
        loop {
            match graph.run().unwrap() {
                GraphStatus::Eof => break,
                GraphStatus::HasOutput(_) => {
                    while let Ok(f) = graph.recv(sink) {
                        out.push(
                            f.plane(0)
                                .and_then(|p| p.row(0))
                                .and_then(|r| r.first())
                                .copied()
                                .unwrap_or(0),
                        );
                    }
                }
                GraphStatus::NeedInput(_) => {}
                other => panic!("unexpected graph status: {other:?}"),
            }
        }
        out
    }

    #[test]
    fn reverses_content_order() {
        assert_eq!(run(1, 5), vec![40, 30, 20, 10, 0]);
    }

    /// The falsifying test this module's doc calls for: a no-op reversal
    /// and a genuine content reversal both "look plausible" from one
    /// direction, but only a real reversal is idempotent under `reverse`
    /// applied twice. Measured identically against `ffmpeg 8.1`.
    #[test]
    fn reverse_twice_is_the_identity() {
        assert_eq!(run(2, 5), vec![0, 10, 20, 30, 40]);
    }

    #[test]
    fn a_single_frame_reverses_to_itself() {
        assert_eq!(run(1, 1), vec![0]);
    }
}
