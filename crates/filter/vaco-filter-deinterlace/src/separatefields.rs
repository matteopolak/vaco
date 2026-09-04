//! `separatefields` — split each input frame into its two fields, doubling
//! the frame rate and halving the height.
//!
//! `ffmpeg -h filter=separatefields`: no options.
//!
//! # Measured: field order and row selection
//!
//! Built a `2x8` gray ramp (`geq=lum='(Y+1)*10'`, one distinct value per row)
//! and ran `separatefields` after `setfield=tff`/`setfield=bff`/no
//! `setfield` at all (`ffmpeg` 8.1, 2026-08-23):
//!
//! ```text
//! setfield=tff  -> field1 = even rows (0,2,4,6), field2 = odd rows (1,3,5,7)
//! setfield=bff  -> field1 = odd rows,            field2 = even rows
//! (unmarked)    -> field1 = odd rows,            field2 = even rows  (behaves like bff)
//! ```
//!
//! So: the first output field is the top field (even rows) when the input
//! is flagged top-field-first, and the bottom field (odd rows) otherwise —
//! including the unmarked/progressive case, which is *not* the same as
//! defaulting to top. [`crate::video::extract_field`] does the row
//! selection; this module only decides the order.
//!
//! # Independent oracle: `separatefields` then `weave` is the identity
//!
//! Rather than trust this module's own row-selection logic a second time,
//! [`crate::weave`]'s tests round-trip a synthetic frame through
//! `separatefields` and `weave` and check the result is byte-identical to
//! the input — the invariant this whole row's brief names explicitly. That
//! is a property of the *pair*, not of either filter's internals, so it
//! cannot pass by both modules sharing the same mistake.

use vaco_core::{MediaType, Result};
use vaco_filter_core::adapt::{FrameFilter, FrameOut, Simple};
use vaco_filter_core::negotiate::NodeFormats;
use vaco_filter_core::{FilterContext, FilterDesc, FilterFlags};
use vaco_frame::Frame;

use vaco_filter_graph::registry::{Instance, Instantiate};

use crate::video::{VIDEO_PAD, is_tff};

pub const DESC: FilterDesc = FilterDesc {
    name: "separatefields",
    description: "Split input video frames into fields.",
    inputs: VIDEO_PAD,
    outputs: VIDEO_PAD,
    flags: FilterFlags::empty(),
};

#[derive(Debug, Default)]
pub(crate) struct Filter;

impl FrameFilter for Filter {
    fn filter_frame(&mut self, ctx: &mut FilterContext<'_>, input: Frame) -> Result<FrameOut> {
        let tff = is_tff(&input);
        let mut first = crate::video::extract_field(ctx.pool(), &input, tff)?;
        let mut second = crate::video::extract_field(ctx.pool(), &input, !tff)?;
        first.pts = input.pts;
        second.pts = input.pts;
        // Halve the duration between the two fields when known; a real
        // per-field PTS offset would need the output link's time base
        // (not verified against the reference here — see the crate docs).
        if input.duration_ticks() > 0 {
            #[allow(
                clippy::integer_division,
                reason = "an approximate half-duration split for display, not an exact-count computation"
            )]
            let half = input.duration_ticks() / 2;
            first.set_duration_ticks(half);
            second.set_duration_ticks(input.duration_ticks().saturating_sub(half));
            if let Some(p) = input.pts.ticks() {
                second.pts = vaco_core::Timestamp::new(p.saturating_add(half));
            }
        }
        Ok(FrameOut::Many(smallvec::smallvec![first, second]))
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
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]
mod tests {
    use crate::video::extract_field;
    use crate::video::test_support::{ramp_frame, row_value};
    use vaco_frame::FramePool;

    #[test]
    fn top_field_first_gives_even_rows_first() {
        // Measured: setfield=tff -> field1 = even rows, field2 = odd rows.
        let mut f = ramp_frame(2, 8);
        f.flags.insert(vaco_frame::FrameFlags::TOP_FIELD_FIRST);
        let pool = FramePool::default();
        let tff = super::is_tff(&f);
        assert!(tff);
        let first = extract_field(&pool, &f, tff).unwrap();
        let second = extract_field(&pool, &f, !tff).unwrap();
        assert_eq!(row_value(&first, 0), 0);
        assert_eq!(row_value(&first, 1), 2);
        assert_eq!(row_value(&second, 0), 1);
        assert_eq!(row_value(&second, 1), 3);
    }

    #[test]
    fn unmarked_behaves_like_bottom_field_first() {
        let f = ramp_frame(2, 8);
        let pool = FramePool::default();
        let tff = super::is_tff(&f);
        assert!(!tff, "unmarked frames measure as bottom-field-first");
        let first = extract_field(&pool, &f, tff).unwrap();
        let second = extract_field(&pool, &f, !tff).unwrap();
        assert_eq!(row_value(&first, 0), 1);
        assert_eq!(row_value(&second, 0), 0);
    }
}
