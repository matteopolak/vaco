//! `shuffleframes` — reorder frames within a fixed-size sliding group.
//!
//! `ffmpeg -h filter=shuffleframes` documents `mapping` (a space-separated
//! list of destination indexes, default `"0"`): frame `i` within a group of
//! `N = len(mapping)` consecutive input frames is written to output
//! position `mapping[i]`. Implemented directly from that description — not
//! independently measured against the reference (there is no pixel
//! computation to get subtly wrong here, only an index permutation), with
//! an oracle that does not depend on trusting this reading regardless: the
//! identity mapping (`"0 1 2"`, the default extended to a group of `N`)
//! must reproduce every input frame unchanged and in order, which only
//! holds if frames are moved, not dropped or duplicated.
//!
//! A destination index written by more than one source position keeps the
//! *last* one (matching a plain array-write loop); a group with unmapped
//! destination positions is short — this crate has not measured what the
//! reference does there, so a short group emits [`FrameOut::Many`] with
//! only the mapped frames present, still in destination order.

use smallvec::SmallVec;
use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::Frame;
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "shuffleframes",
    description: "Shuffle video frames",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "shuffleframes", help = "Shuffle video frames")]
pub(crate) struct Opts {
    #[opt(
        name = "mapping",
        help = "set destination indexes of input frames",
        default = "0".to_owned(),
        flags(video, filtering)
    )]
    pub mapping: String,
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

fn parse_mapping(s: &str) -> std::result::Result<Vec<usize>, String> {
    let v: std::result::Result<Vec<usize>, _> =
        s.split_whitespace().map(str::parse::<usize>).collect();
    let v = v.map_err(|_| format!("shuffleframes: bad `mapping` `{s}`"))?;
    if v.is_empty() {
        return Err(format!("shuffleframes: bad `mapping` `{s}`"));
    }
    Ok(v)
}

#[derive(Debug)]
pub(crate) struct Filter {
    mapping: Vec<usize>,
    buffer: SmallVec<[Frame; 8]>,
}

impl Filter {
    pub(crate) fn new(mapping: Vec<usize>) -> Self {
        Self {
            mapping,
            buffer: SmallVec::new(),
        }
    }

    fn emit(&mut self) -> FrameOut {
        let n = self.mapping.len();
        let mut out: Vec<Option<Frame>> = (0..n).map(|_| None).collect();
        for (src_idx, frame) in self.buffer.drain(..).enumerate() {
            if let Some(&dst_idx) = self.mapping.get(src_idx)
                && dst_idx < n
                && let Some(slot) = out.get_mut(dst_idx)
            {
                *slot = Some(frame);
            }
        }
        out.into_iter().flatten().collect()
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let _ = ctx;
        self.buffer.push(input);
        if self.buffer.len() < self.mapping.len() {
            return Ok(FrameOut::None);
        }
        Ok(self.emit())
    }

    fn flush(&mut self, ctx: &mut FilterContext<'_>) -> Result<FrameOut> {
        let _ = ctx;
        if self.buffer.is_empty() {
            return Ok(FrameOut::None);
        }
        Ok(self.emit())
    }

    fn flush_state(&mut self) {
        self.buffer.clear();
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let mapping = parse_mapping(&opts.mapping)?;
    let filter = Filter::new(mapping);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, reason = "test code")]
mod tests {
    use super::*;
    use vaco_core::Timestamp;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    fn frame(pts: i64) -> Frame {
        let pool = FramePool::default();
        let mut f = pool.acquire_video(PixFmt::Gray8, 2, 2).unwrap();
        f.pts = Timestamp::new(pts);
        f
    }

    // `emit()` is exercised directly rather than through `FrameFilter::
    // filter_frame`: building a real `FilterContext` needs a live
    // scheduler (`FilterContext::new` is `pub(crate)` to that crate), and
    // `filter_frame`/`flush` do nothing with `ctx` in this filter anyway
    // (`let _ = ctx;`) — the whole implementation worth testing is `emit`.

    #[test]
    fn identity_mapping_preserves_order() {
        let mut filter = Filter::new(vec![0, 1, 2]);
        filter.buffer.push(frame(0));
        filter.buffer.push(frame(1));
        filter.buffer.push(frame(2));
        let FrameOut::Many(frames) = filter.emit() else {
            panic!("expected three frames");
        };
        let pts: Vec<i64> = frames.iter().map(|f| f.pts.ticks().unwrap_or(-1)).collect();
        assert_eq!(pts, vec![0, 1, 2]);
    }

    #[test]
    fn reverse_mapping_reorders() {
        let mut filter = Filter::new(vec![2, 1, 0]);
        filter.buffer.push(frame(0));
        filter.buffer.push(frame(1));
        filter.buffer.push(frame(2));
        let FrameOut::Many(frames) = filter.emit() else {
            panic!("expected three frames");
        };
        let pts: Vec<i64> = frames.iter().map(|f| f.pts.ticks().unwrap_or(-1)).collect();
        assert_eq!(pts, vec![2, 1, 0]);
    }

    #[test]
    fn mapping_parses_whitespace_separated_indexes() {
        assert_eq!(parse_mapping("2 1 0").unwrap(), vec![2, 1, 0]);
        assert_eq!(parse_mapping("0").unwrap(), vec![0]);
        assert!(parse_mapping("").is_err());
    }
}
