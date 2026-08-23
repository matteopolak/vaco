//! `repeatfields` — hard-duplicate a field when the source signalled MPEG-2
//! "repeat first field" (3:2 pulldown baked into the bitstream rather than
//! reconstructed by a filter).
//!
//! `ffmpeg -h filter=repeatfields`: no options.
//!
//! # A real, structural gap: there is nowhere to read the flag from
//!
//! The reference's `repeatfields` reads `AVFrame::repeat_pict`, set by the
//! MPEG-1/2 decoder from the picture header's `repeat_first_field` bit
//! (`top_field_first` + `repeat_first_field` together select 2, 3 or 4
//! fields of display time). `vaco_frame::Frame`/`FrameFlags`
//! (`crates/model/vaco-frame`, not owned by this crate) has no equivalent —
//! only `INTERLACED` and `TOP_FIELD_FIRST` exist, both booleans, with no
//! room for "repeat by how many extra fields". `vaco-filter-deinterlace`
//! does not own `vaco-frame` and this brief's single-writer rule forbids
//! editing it, so this is recorded here rather than worked around: the
//! filter is registered and does not panic on any input, but since no input
//! it can see ever carries a repeat signal, it behaves as if
//! `repeat_pict==0` for every frame — a pure passthrough. That is
//! observably different from the reference on any stream a real MPEG-2
//! decoder would flag, and is not a place-holder that will quietly start
//! doing the right thing later: closing it needs a field this crate cannot
//! add.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::VIDEO_PAD;

pub const DESC: FilterDesc = FilterDesc {
    name: "repeatfields",
    description: "Hard repeat fields based on MPEG repeat field flag.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let _ = ctx;
        // See the module doc: no input this crate can observe ever carries
        // a repeat-field signal, so every frame passes through unchanged.
        Ok(FrameOut::One(input))
    }
}

pub(crate) fn create(req: &Instantiate<'_>) -> Instance {
    let _ = req;
    Instance {
        desc: DESC,
        formats: NodeFormats::passthrough(1, 1, MediaType::Video, req.instance),
        filter: Box::new(Simple::new(Filter)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_under_the_measured_name() {
        assert_eq!(DESC.name, "repeatfields");
        assert!(DESC.description.contains("repeat"));
    }
}
