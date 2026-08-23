//! `image2` mux: FM-35b (issue #593).
//!
//! # What this crate is
//!
//! The write-side counterpart to `vaco-demux-image2`: filename patterns,
//! `-update`, `-strftime`, `-frame_pts` and `-atomic_writing`.
//!
//! * [`writer::Image2MuxWriter`] is the real thing — a path pattern plus
//!   [`writer::Image2MuxOptions`], one file per frame (or one file total
//!   under `-update`).
//! * [`pipe_mux::MUXER_IMAGE2`] is the registry-reachable degenerate case:
//!   [`vaco_format_core::MuxerDesc::open`]'s frozen signature has nowhere to
//!   put a filename pattern (see [`writer`]'s module docs), so the registry
//!   path gets `image2pipe`'s shape instead — every frame written back to
//!   back into the one sink it is given.
//!
//! # `-strftime`'s wall clock
//!
//! [`strftime`] never calls `std::time::SystemTime::now()` directly — that
//! panics on `wasm32-unknown-unknown` — routing instead through
//! `vaco_time::unix_nanos()`, which is total (returns `None` rather than
//! panicking) on every target. See that module's docs for the directive
//! subset implemented and the calendar algorithm used.
//!
//! # Dependencies
//!
//! Depends on `vaco-demux-image2` (a sibling format crate, not a layering
//! violation under D14.1, which is specifically about a format crate
//! depending on a codec/parser crate) for [`vaco_demux_image2::pattern`]'s
//! `%d`/`%0Nd` sequence-pattern logic, so the two crates' filename grammar
//! cannot drift apart the way it would if each reimplemented it.

#![forbid(unsafe_code)]

pub mod pipe_mux;
pub mod strftime;
pub mod writer;

pub use pipe_mux::MUXER_IMAGE2;
pub use writer::{Image2MuxOptions, Image2MuxWriter};

use vaco_format_core::MuxerDesc;

/// Every muxer this crate registers.
#[must_use]
pub fn all_muxers() -> Vec<&'static MuxerDesc> {
    vec![&MUXER_IMAGE2]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test code")]
mod tests {
    use super::*;

    #[test]
    fn there_is_exactly_one_registration() {
        let muxers = all_muxers();
        assert_eq!(muxers.len(), 1);
        assert_eq!(muxers.first().unwrap().name, "image2");
    }
}
