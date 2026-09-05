//! Utility sink muxers: `null` and `mkvtimestamp_v2`.
//!
//! [`null`] discards every packet. [`mkvtimestamp_v2`] writes a header followed
//! by one PTS-in-ms line per video frame.
//!
//! `uncodedframecrc` is not included because hashing decoded frames requires
//! per-frame geometry that [`vaco_format_core::Muxer::write_packet`] does not
//! carry.

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
