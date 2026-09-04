//! `image2` and its 37 `*_pipe` splitters: filesystem patterns and byte framing.
//!
//! # What this crate is
//!
//! Two different jobs the reference happens to group under one family name:
//!
//! * **`image2`** ([`multi::Image2Demuxer`]): a filename *pattern*
//!   (`out%03d.png`, `out*.png`) resolved against the filesystem, one whole
//!   file per packet. This is the only demuxer in this crate that touches a
//!   filesystem at all.
//! * **37 `*_pipe` splitters** ([`pipe`]): find image boundaries in a
//!   *byte stream* that may hold several images back to back — a decoder's
//!   job's prerequisite, not the decoder's job itself. Framing lives here,
//!   in the format layer, per D14.1; see [`pipe::framing`] for exactly which
//!   of the 37 have real per-image framing versus the reference's own
//!   whole-input fallback.
//!
//! Not registered: `image2pipe` (a 38th name in `ffmpeg -demuxers`, but a
//! content-sniffing dispatcher over the 37 splitters rather than a splitter
//! of its own — this crate's scope covers "all 42 [sic; actually 37] pipe
//! splitters," not this dispatcher, and it was left out rather than rushed;
//! see the crate's issue-closing comment for the honest accounting) and
//! `yuv4mpegpipe` (registered by `vaco-demux-raw`, matching the reference's
//! own module boundary between `img2dec.c` and `yuv4mpegdec.c`).
//!
//! # Why filesystem access and byte framing are different modules
//!
//! [`pattern`] (sequence numbering) and [`glob`] (glob matching) are pure
//! string algorithms with no I/O, so they are the half of this crate that
//! still does something useful if `vaco-cli` is ever built for
//! `wasm32-unknown-unknown`: pattern expansion for a UI, say, with the actual
//! file access supplied by a host binding this crate does not assume.
//! [`fsutil`] is the other half — `std::fs`, which *compiles* for that target
//! and returns an I/O error at every call at runtime, because there is no
//! filesystem underneath it there. Keeping the split at the module boundary
//! (rather than behind a `#[cfg]`) is what makes that true without a special
//! build: `cargo xtask wasm-check` compiles this crate as-is.
//!
//! # Layering (D14.1)
//!
//! No dependency on any `vaco-parse-*`/`vaco-codec-<name>` crate. The 37
//! splitters name a [`vaco_codec_core::CodecId`] where one exists (`png`,
//! `jpeg`, `gif`, `bmp`, `tiff`, `webp` — six of thirty-seven) and otherwise
//! carry the reference's exact name as `raw_codec_name` stream metadata,
//! `vaco-demux-raw`'s own documented convention for the same gap.

#![forbid(unsafe_code)]

pub mod fsutil;
pub mod glob;
pub mod multi;
pub mod pattern;
pub mod pipe;

pub use multi::{DEMUXER_IMAGE2, Image2Demuxer, Image2Options, PatternType, TsFromFile};
pub use pipe::PIPE_DEMUXERS;

use vaco_format_core::DemuxerDesc;

/// Every demuxer this crate registers: `image2` plus the 37 pipe splitters.
/// Matches [`vaco-demux-raw`](../vaco_demux_raw/index.html)'s
/// `all_demuxers()` convention, and is what this crate's own registration
/// tests check against `vaco-component.toml`'s row count.
#[must_use]
pub fn all_demuxers() -> Vec<&'static DemuxerDesc> {
    let mut out = vec![&DEMUXER_IMAGE2];
    out.extend(pipe::PIPE_DEMUXERS.iter().copied());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_thirty_eight_registrations() {
        // image2 + 37 pipe splitters.
        assert_eq!(all_demuxers().len(), 38);
    }

    #[test]
    fn every_name_is_unique() {
        let all = all_demuxers();
        let mut names: Vec<&str> = all.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate demuxer name registered");
    }

    #[test]
    fn every_probe_is_total_over_hostile_buffers() {
        use vaco_format_core::probe::ProbeData;
        for d in all_demuxers() {
            let _ = (d.probe)(&ProbeData::new(&[]));
            let _ = (d.probe)(&ProbeData::new(&[0u8; 128]));
        }
    }
}
