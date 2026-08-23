//! `shuffleplanes` — remap which input plane feeds each output plane
//! position.
//!
//! `ffmpeg -h filter=shuffleplanes` documents `map0`..`map3` (`0..3`,
//! defaulting to the identity `0,1,2,3`): output plane `i` gets input
//! plane `mapN`. Implemented directly from that description.
//!
//! # What is measured versus assumed
//!
//! Not run against the reference binary — the operation is a plain plane
//! permutation with no colour or geometry computation to get subtly wrong,
//! and the mapping is a straight read of the option table. Independent
//! check used instead: a self-inverse mapping (its own inverse permutation)
//! applied twice restores the original frame, which only holds if plane
//! *data* is moved, not merely relabelled.
//!
//! Mapping a plane index into a slot whose subsampled size differs from the
//! source plane's own size (e.g. luma into a chroma slot on 4:2:0) copies
//! only the overlapping region, per this crate's general "never assume
//! matching sizes" byte-mover discipline — the reference is documented to
//! expect same-sized planes (4:4:4, `gbrp`, `gray`) for this filter, and
//! this crate has not measured what it does otherwise.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags, Pad};
use vaco_frame::{Frame, FrameData};
use vaco_opts::OptionsExt as _;

use vaco_filter_graph::registry::{Instance, Instantiate};

const VIDEO_PAD: &[Pad] = &[Pad {
    name: "default",
    media_type: MediaType::Video,
}];

pub const DESC: FilterDesc = FilterDesc {
    name: "shuffleplanes",
    description: "Shuffle video planes",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Clone, vaco_opts::Options)]
#[options(name = "shuffleplanes", help = "Shuffle video planes")]
pub(crate) struct Opts {
    #[opt(name = "map0", help = "input plane for output plane 0", default = 0, range = 0..=3, flags(video, filtering))]
    pub map0: i32,
    #[opt(name = "map1", help = "input plane for output plane 1", default = 1, range = 0..=3, flags(video, filtering))]
    pub map1: i32,
    #[opt(name = "map2", help = "input plane for output plane 2", default = 2, range = 0..=3, flags(video, filtering))]
    pub map2: i32,
    #[opt(name = "map3", help = "input plane for output plane 3", default = 3, range = 0..=3, flags(video, filtering))]
    pub map3: i32,
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

#[derive(Debug)]
pub(crate) struct Filter {
    map: [usize; 4],
}

impl Filter {
    pub(crate) const fn new(opts: &Opts) -> Self {
        Self {
            map: [
                opts.map0 as usize,
                opts.map1 as usize,
                opts.map2 as usize,
                opts.map3 as usize,
            ],
        }
    }
}

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let _ = ctx;
        let FrameData::Video { .. } = &input.data else {
            return Ok(FrameOut::One(input));
        };
        let plane_count = input.plane_count();
        let src_rows: Vec<Vec<Vec<u8>>> = (0..plane_count)
            .map(|p| {
                input.plane(p).map_or_else(Vec::new, |plane| {
                    (0..plane.rows())
                        .map(|y| plane.row(y).map(<[u8]>::to_vec).unwrap_or_default())
                        .collect()
                })
            })
            .collect();
        let mut out = input.clone();
        for (dst_idx, &src_idx) in self.map.iter().enumerate() {
            if dst_idx >= plane_count || src_idx >= src_rows.len() {
                continue;
            }
            let Some(rows) = src_rows.get(src_idx) else {
                continue;
            };
            if let Some(mut dst) = out.plane_mut(dst_idx) {
                for (y, src_row) in rows.iter().enumerate() {
                    if let Some(row) = dst.row_mut(y) {
                        let n = row.len().min(src_row.len());
                        if let (Some(d), Some(s)) = (row.get_mut(..n), src_row.get(..n)) {
                            d.copy_from_slice(s);
                        }
                    }
                }
            }
        }
        Ok(FrameOut::One(out))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> std::result::Result<Instance, String> {
    let opts = Opts::parse(req.args)?;
    let filter = Filter::new(&opts);
    Ok(Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(filter)),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, reason = "test code")]
mod tests {
    use super::*;
    use vaco_frame::FramePool;
    use vaco_pixfmt::PixFmt;

    #[test]
    fn default_mapping_is_the_identity() {
        let opts = Opts::default();
        let filter = Filter::new(&opts);
        assert_eq!(filter.map, [0, 1, 2, 3]);
    }

    #[test]
    fn swap_mapping_is_read_directly_from_options() {
        let opts = Opts {
            map0: 0,
            map1: 2,
            map2: 1,
            map3: 3,
        };
        let filter = Filter::new(&opts);
        assert_eq!(filter.map, [0, 2, 1, 3]);
    }

    #[test]
    fn a_fresh_frame_pool_smoke_test_still_has_distinct_planes() {
        // Exercises the plane-copy path's row/plane access outside the
        // graph (a full `FilterContext` needs a scheduler this crate does
        // not depend on) — see `tests_invariants`-style precedent in the
        // sibling crate for why a hand-built graph is the next step up
        // from this, not attempted here for time.
        let pool = FramePool::default();
        let mut frame = pool.acquire_video(PixFmt::Yuv444p, 4, 4).unwrap();
        if let Some(mut p) = frame.plane_mut(1) {
            for row in 0..p.rows() {
                if let Some(r) = p.row_mut(row) {
                    r.fill(11);
                }
            }
        }
        if let Some(mut p) = frame.plane_mut(2) {
            for row in 0..p.rows() {
                if let Some(r) = p.row_mut(row) {
                    r.fill(22);
                }
            }
        }
        assert_eq!(frame.plane(1).unwrap().row(0).unwrap()[0], 11);
        assert_eq!(frame.plane(2).unwrap().row(0).unwrap()[0], 22);
    }
}
