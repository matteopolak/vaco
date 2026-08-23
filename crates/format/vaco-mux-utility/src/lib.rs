//! Utility sink muxers: `null` and `mkvtimestamp_v2` (FM-20, issue #572).
//!
//! # What this crate is
//!
//! The two registrations issue #572 asks for that
//! [`vaco-mux-hash`](../vaco_mux_hash/index.html) did not implement: terminal
//! sinks that either discard every byte ([`null`]) or dump one plain-text
//! line per frame ([`mkvtimestamp_v2`]). Neither owns another muxer and
//! neither reads untrusted input, which is what separates this crate from
//! `vaco-mux-stream`'s meta-muxers (`segment`, `tee`, `fifo`, …) — see that
//! crate's docs for why the two were split rather than merged.
//!
//! `uncodedframecrc`, the third registration issue #572 names, is **not**
//! implemented here. It hashes decoded frames and needs per-frame geometry
//! that [`vaco_format_core::Muxer::write_packet`] has no channel for — see
//! `docs/format/vaco-mux-utility.md` for the full accounting, which matches
//! `vaco-mux-hash`'s documented reasoning for the same gap.
//!
//! # Layout
//!
//! | Module | Registration | Shape |
//! |---|---|---|
//! | [`null`] | `null` | discards every packet |
//! | [`mkvtimestamp`] | `mkvtimestamp_v2` | header + one PTS-in-ms line per video frame |

#![forbid(unsafe_code)]

pub mod mkvtimestamp;
pub mod null;

pub use mkvtimestamp::MUXER_MKVTIMESTAMP_V2;
pub use null::MUXER_NULL;

use vaco_format_core::MuxerDesc;

/// Every muxer this crate registers.
#[must_use]
pub fn all_muxers() -> Vec<&'static MuxerDesc> {
    vec![&MUXER_NULL, &MUXER_MKVTIMESTAMP_V2]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_exactly_two_registrations() {
        assert_eq!(all_muxers().len(), 2);
    }

    #[test]
    fn every_name_is_unique() {
        let all = all_muxers();
        let mut names: Vec<&str> = all.iter().map(|d| d.name).collect();
        names.sort_unstable();
        let mut dedup = names.clone();
        dedup.dedup();
        assert_eq!(names, dedup, "duplicate muxer name registered");
    }

    #[test]
    fn every_descriptor_opens() {
        use vaco_format_core::vacoraw::MemorySink;
        for desc in all_muxers() {
            let sink = Box::new(MemorySink::new());
            assert!((desc.open)(sink).is_ok(), "{} failed to open", desc.name);
        }
    }
}
