//! The `abst` "bootstrap info" box: one per quality level, naming the
//! segment/fragment addressing scheme a client uses to find a given
//! fragment's file. This is the hard part of HDS — a wrong reading here
//! produces a file that parses cleanly and resolves to the wrong fragment,
//! not a file that fails to parse — so every field below is a direct,
//! independent byte-offset measurement against a real reference file
//! (`hds-samples/out12.f4m/stream0.abst`, `provenance/sources.toml`'s
//! `ffmpeg-hds-mux-probe` entry), not derived from the (historically Adobe,
//! not freely republished) HDS bootstrap-box text.
//!
//! # Measured layout (122-byte reference file, one segment of two fragments)
//!
//! ```text
//! abst (full box, version 0)
//!   bootstrapinfoVersion   u32        = 2 (measured; this crate emits 2 too)
//!   profile/live/update    u8         = 0 (named profile, not live, no update)
//!   timescale              u32        = 1000  (milliseconds — NOT HNS; see lib.rs)
//!   currentMediaTime       u64        total duration so far, in `timescale` ticks
//!   smpteTimeCodeOffset    u64        = 0 (never set by this crate)
//!   movieIdentifier        UTF8, null-terminated  = "" (empty)
//!   serverEntryCount       u8         = 0
//!   qualityEntryCount      u8         = 0
//!   drmData                UTF8, null-terminated  = "" (empty)
//!   metaData               UTF8, null-terminated  = "" (empty)
//!   segmentRunTableCount   u8         = 1
//!   segmentRunTableEntries[1] = one `asrt` (full box, version 0):
//!     qualityEntryCount    u8         = 0
//!     segmentRunEntryCount u32        = 1
//!     entries[1]: firstSegment u32 = 1, fragmentsPerSegment u32 = <total fragments>
//!   fragmentRunTableCount  u8         = 1
//!   fragmentRunTableEntries[1] = one `afrt` (full box, version 0):
//!     timescale            u32        = 1000
//!     qualityEntryCount    u8         = 0
//!     fragmentRunEntryCount u32       = <fragment count>
//!     entries[N]: firstFragment u32, firstFragmentTimestamp u64, fragmentDuration u32
//!       (a `discontinuityIndicator` byte only follows when `fragmentDuration`
//!       is 0 — never emitted here, since every fragment this crate writes
//!       has a real, positive measured duration)
//! ```
//!
//! Every fragment this crate ever writes lands in segment 1 — the reference
//! never produced a second segment at the durations/`-window_size 0` this
//! project measured it at, and `-window_size`/`-extra_window_size` are a
//! live-streaming sliding-window concern this crate does not implement (see
//! `lib.rs`).

use vaco_format_isom::build::fullbx;

pub const TIMESCALE: u32 = 1000;

/// One completed fragment, for the `afrt` run table.
#[derive(Debug, Clone, Copy)]
pub struct FragmentRun {
    pub first_fragment: u32,
    pub first_fragment_timestamp_ms: u64,
    pub duration_ms: u32,
}

/// Build the complete `abst` box for one quality level.
#[must_use]
pub fn build_abst(current_media_time_ms: u64, fragments: &[FragmentRun]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&2u32.to_be_bytes()); // bootstrapinfoVersion
    payload.push(0x00); // profile=named, live=false, update=false
    payload.extend_from_slice(&TIMESCALE.to_be_bytes());
    payload.extend_from_slice(&current_media_time_ms.to_be_bytes());
    payload.extend_from_slice(&0u64.to_be_bytes()); // smpteTimeCodeOffset
    payload.push(0x00); // movieIdentifier (empty)
    payload.push(0x00); // serverEntryCount
    payload.push(0x00); // qualityEntryCount
    payload.push(0x00); // drmData (empty)
    payload.push(0x00); // metaData (empty)
    payload.push(0x01); // segmentRunTableCount
    payload.extend_from_slice(&build_asrt(fragment_count_u32(fragments)));
    payload.push(0x01); // fragmentRunTableCount
    payload.extend_from_slice(&build_afrt(fragments));
    fullbx(b"abst", 0, 0, &payload)
}

fn fragment_count_u32(fragments: &[FragmentRun]) -> u32 {
    u32::try_from(fragments.len()).unwrap_or(u32::MAX)
}

fn build_asrt(fragments_per_segment: u32) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.push(0x00); // qualityEntryCount
    payload.extend_from_slice(&1u32.to_be_bytes()); // segmentRunEntryCount
    payload.extend_from_slice(&1u32.to_be_bytes()); // firstSegment (always 1, see module docs)
    payload.extend_from_slice(&fragments_per_segment.to_be_bytes());
    fullbx(b"asrt", 0, 0, &payload)
}

fn build_afrt(fragments: &[FragmentRun]) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&TIMESCALE.to_be_bytes());
    payload.push(0x00); // qualityEntryCount
    payload.extend_from_slice(&fragment_count_u32(fragments).to_be_bytes());
    for f in fragments {
        payload.extend_from_slice(&f.first_fragment.to_be_bytes());
        payload.extend_from_slice(&f.first_fragment_timestamp_ms.to_be_bytes());
        payload.extend_from_slice(&f.duration_ms.to_be_bytes());
    }
    fullbx(b"afrt", 0, 0, &payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Field-for-field against `hds-samples/out12.f4m/stream0.abst`'s own
    /// 122 measured bytes.
    #[test]
    fn matches_the_measured_two_fragment_reference_exactly() {
        let fragments = [
            FragmentRun {
                first_fragment: 1,
                first_fragment_timestamp_ms: 0,
                duration_ms: 10_000,
            },
            FragmentRun {
                first_fragment: 2,
                first_fragment_timestamp_ms: 10_000,
                duration_ms: 2_069,
            },
        ];
        let abst = build_abst(12_069, &fragments);
        let expected = "0000007a61627374000000000000000200000003e80000000000002f2500000000000000000000000000010000001961737274000000000000000001000000010000000201000000356166727400000000000003e800000000020000000100000000000000000000271000000002000000000000271000000815";
        assert_eq!(hex(&abst), expected);
    }

    fn hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}
